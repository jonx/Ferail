//! Windows shell integration for Ferail.
//!
//! Mirrors the public surface of `ferail-shell-mac` so the
//! `platform_shell` cfg-alias in `ferail-gpui` can swap one for the
//! other based on target OS. Each function has a `cfg(windows)` arm
//! (today a stub; future home of real Win32 implementations ported
//! from the Ferail predecessor's `crates/ferail-win32/`) and a
//! `cfg(not(windows))` no-op arm so the workspace `cargo check` keeps
//! compiling this crate on macOS / Linux dev hosts.
//!
//! The winit-window-taking subset of shell-mac (`begin_drag`,
//! `show_context_menu`, `install_services_anchor`, `show_share_picker`,
//! `apply_native_chrome`) is intentionally **omitted**: those signatures
//! are not called from `ferail-gpui`. If a future GPUI Windows
//! surface needs them,
//! they grow back through the `windows` crate's HWND, not through
//! winit.
//!
//! Stub bodies match shell-mac's non-macOS arms (no-op / `false` /
//! `Vec::new()` / `Err("...stub")`) so the existing call sites' error
//! handling continues to apply unchanged.

#[cfg(windows)]
mod preview_handler;

// Headless-screenshot capture via PrintWindow. macOS goes through
// gpui_macos's MetalRenderer; gpui_windows has no equivalent so
// the harness routes through this module instead.
#[cfg(windows)]
mod capture;
#[cfg(windows)]
pub use capture::{capture_window_rgba, hide_window_for_capture, present_offscreen_for_capture};

// Native video provider (Media Foundation IMFMediaEngine) behind the
// windowless frame-pull contract. macOS uses AVFoundation; this is its
// Windows analogue.
#[cfg(windows)]
mod video_mf;

// =============================================================
// Path helpers
// =============================================================

/// Convert a verbatim (extended-length) path to the plain form the
/// Windows *shell* APIs accept: `\\?\C:\…` → `C:\…` and
/// `\\?\UNC\server\share\…` → `\\server\share\…`. Non-verbatim paths
/// come back unchanged.
///
/// `std::fs::canonicalize` returns verbatim paths on Windows, and the
/// favorites / tabs / persisted paths the host hands us carry that
/// prefix — but `explorer /select`, `wt.exe -d`, CF_HDROP, `IShellLink::
/// SetPath`, `SHCreateItemFromParsingName`, `SHGetFileInfoW`, and the
/// Media Foundation source URL all reject or mis-handle `\\?\`. Every
/// outward shell boundary in this crate routes through this helper.
///
/// Pure string logic (no I/O), compiled on every host so the unit tests
/// below run on non-Windows dev machines too. Paths that are not valid
/// UTF-8 (unpaired surrogates) are returned unchanged rather than
/// lossily rewritten.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn strip_verbatim(path: &std::path::Path) -> std::path::PathBuf {
    let Some(s) = path.to_str() else {
        return path.to_path_buf();
    };
    let Some(rest) = s.strip_prefix(r"\\?\") else {
        return path.to_path_buf();
    };
    // `\\?\UNC\server\share\…` → `\\server\share\…` (prefix is
    // case-insensitive, matching the kernel's own parsing).
    let b = rest.as_bytes();
    if b.len() > 4
        && b[0].eq_ignore_ascii_case(&b'U')
        && b[1].eq_ignore_ascii_case(&b'N')
        && b[2].eq_ignore_ascii_case(&b'C')
        && b[3] == b'\\'
    {
        return std::path::PathBuf::from(format!(r"\\{}", &rest[4..]));
    }
    std::path::PathBuf::from(rest)
}

#[cfg(test)]
mod strip_verbatim_tests {
    use super::strip_verbatim;
    use std::path::Path;

    #[test]
    fn plain_drive_path_unchanged() {
        let p = Path::new(r"C:\Users\jkn\file.txt");
        assert_eq!(strip_verbatim(p), p);
    }

    #[test]
    fn drive_verbatim_stripped() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\C:\Users\jkn\file.txt")),
            Path::new(r"C:\Users\jkn\file.txt")
        );
    }

    #[test]
    fn unc_verbatim_rewritten() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\server\share\dir\file.txt")),
            Path::new(r"\\server\share\dir\file.txt")
        );
        // Prefix match is case-insensitive.
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\unc\server\share")),
            Path::new(r"\\server\share")
        );
    }

    #[test]
    fn non_verbatim_unc_unchanged() {
        let p = Path::new(r"\\server\share\dir");
        assert_eq!(strip_verbatim(p), p);
    }
}

// =============================================================
// Types — defined unconditionally so callers can name them on
// either platform; the shell-mac equivalents have the same shape.
// =============================================================

/// One app the system would offer in the Open With submenu for a
/// given file. Shape mirrored from `ferail-shell-mac::OpenWithCandidate`.
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

/// Get Info facts for one path that come from the OS shell rather than
/// POSIX `stat`. Shape mirrored from `ferail_shell_mac::ShellInfo` so the
/// Get Info panel ([`ferail-gpui/src/entry_info.rs`]) reads one type
/// through the `platform_shell` alias on every OS.
///
/// On Windows most of these have no exact analogue: there is no per-file
/// "uniform type identifier", no Finder "date added to folder", and the
/// "hide known extension" toggle is a global Explorer setting, not a
/// per-file bit. The Get Info panel falls back to magic detection for the
/// "Kind" row when `kind` is `None`, so a default value is harmless. A
/// richer Windows fill (e.g. `kind` from `SHGetFileInfo`/`SHGFI_TYPENAME`)
/// can land later without touching callers.
#[derive(Clone, Debug, Default)]
pub struct ShellInfo {
    /// Uniform type identifier (macOS only) — always `None` on Windows.
    pub uti: Option<String>,
    /// Localized type description, e.g. "PNG image".
    pub kind: Option<String>,
    /// When the item was added to its folder (macOS only) — `None` here.
    pub added_unix: Option<i64>,
    /// "Hide extension" state (macOS per-file flag) — `None` on Windows.
    pub hidden_extension: Option<bool>,
    /// True for package/bundle directories (macOS) — `None` on Windows.
    pub is_package: Option<bool>,
    /// True for alias files (macOS) — `None` on Windows (`.lnk` is a file).
    pub is_alias: Option<bool>,
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
    _cb: Option<Box<dyn Fn(ferail_core::commands::CommandId) + 'static>>,
) {
}

/// Process-wide About-panel content. Populated by
/// [`set_about_options`]; read by [`show_about_panel`]. A future
/// gpui modal will read the same struct.
///
/// The fields are only READ under cfg(windows) (`show_about_panel`'s
/// real arm); the unconditional writer keeps the API symmetric, so
/// non-Windows builds see write-only fields — expected, not dead.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Default, Clone)]
struct AboutInfo {
    app_name: String,
    tagline: String,
    version: String,
    copyright: String,
}

static ABOUT_INFO: std::sync::Mutex<Option<AboutInfo>> = std::sync::Mutex::new(None);

/// Configure About-panel content. Stored process-wide for the next
/// [`show_about_panel`] call.
pub fn set_about_options(app_name: &str, tagline: &str, version: &str, copyright: &str) {
    if let Ok(mut slot) = ABOUT_INFO.lock() {
        *slot = Some(AboutInfo {
            app_name: app_name.to_string(),
            tagline: tagline.to_string(),
            version: version.to_string(),
            copyright: copyright.to_string(),
        });
    }
}

/// Show an About dialog. v1 uses a system `MessageBoxW` so the menu
/// item works end-to-end without a custom modal. A future gpui-rendered
/// About pane is the proper replacement; this is the cheap version.
#[cfg(windows)]
pub fn show_about_panel() {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};

    let info = ABOUT_INFO
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .unwrap_or_default();
    let title = if info.app_name.is_empty() {
        ferail_core::tr!("About").into_string()
    } else {
        ferail_core::tr!("About {app_name}", app_name = info.app_name).into_string()
    };
    let body = {
        let mut parts: Vec<String> = Vec::new();
        if !info.app_name.is_empty() {
            parts.push(info.app_name.clone());
        }
        if !info.tagline.is_empty() {
            parts.push(info.tagline.clone());
        }
        if !info.version.is_empty() {
            parts.push(ferail_core::tr!("Version {version}", version = info.version).into_string());
        }
        if !info.copyright.is_empty() {
            parts.push(info.copyright.clone());
        }
        if parts.is_empty() {
            "Ferail".to_string()
        } else {
            parts.join("\n\n")
        }
    };
    let title_w: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let body_w: Vec<u16> = body.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(
            HWND::default(),
            PCWSTR::from_raw(body_w.as_ptr()),
            PCWSTR::from_raw(title_w.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

#[cfg(not(windows))]
pub fn show_about_panel() {}

/// Update the menu's snapshot of the host's tab count.
pub fn set_tab_count(_n: usize) {}

/// Set a command's checkmark state in the menu.
pub fn set_command_state(_id: ferail_core::commands::CommandId, _on: bool) {}

// =============================================================
// Alerts / prompts / URLs
// =============================================================

/// Show a modal alert. macOS uses `NSAlert`; Windows: `MessageBoxW`
/// with the information icon and a single OK button. Synchronous —
/// blocks the calling thread while the dialog is up, matching the
/// shell-mac contract.
#[cfg(windows)]
pub fn show_alert(title: &str, body: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
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

/// Present a native folder picker and return the chosen directory, or
/// `None` if cancelled. Mirrors `ferail_shell_mac::pick_folder` so the
/// Favorites "Locate…" repoint flow (`docs/features/FAVORITES.md` §8.2) is
/// cross-platform. The modern `IFileOpenDialog` with `FOS_PICKFOLDERS` (the
/// Vista+ common item dialog) — user-cancel comes back as an error HRESULT,
/// which maps cleanly to `None` (the caller leaves the favorite untouched).
#[cfg(windows)]
pub fn pick_folder() -> Option<std::path::PathBuf> {
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
    };

    unsafe {
        let co = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let we_initialized = co.is_ok();

        let chosen = (|| -> Option<std::path::PathBuf> {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            let opts = dialog.GetOptions().ok()?;
            dialog
                .SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
                .ok()?;
            // No owner HWND; user-cancel returns an error HRESULT → None.
            dialog.Show(None).ok()?;
            let item = dialog.GetResult().ok()?;
            let pwstr = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let path = pwstr.to_string().ok().map(std::path::PathBuf::from);
            CoTaskMemFree(Some(pwstr.0 as *const core::ffi::c_void));
            path
        })();

        if we_initialized {
            CoUninitialize();
        }
        chosen
    }
}

#[cfg(not(windows))]
pub fn pick_folder() -> Option<std::path::PathBuf> {
    None
}

/// Open a URL in its registered Windows handler. This deliberately uses the
/// Shell API rather than `cmd /C start`: `cmd` adds a second quoting/parser
/// layer and can reinterpret metacharacters in an otherwise valid URL.
#[cfg(windows)]
pub fn open_url(url: &str) {
    let _ = shell_execute_open(std::ffi::OsStr::new(url), None);
}

#[cfg(not(windows))]
pub fn open_url(_url: &str) {}

/// Open a filesystem item with its registered `open` verb. Shell-facing paths
/// are converted out of Rust's `\\?\` canonical form first; the filesystem
/// keeps the verbatim form internally for long-path correctness.
#[cfg(windows)]
pub fn open_with_default(path: &std::path::Path) -> std::io::Result<()> {
    let cleaned = strip_verbatim(path);
    let working_dir = cleaned.parent().map(std::path::Path::as_os_str);
    shell_execute_open(cleaned.as_os_str(), working_dir)
}

#[cfg(not(windows))]
pub fn open_with_default(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn shell_execute_open(
    target: &std::ffi::OsStr,
    working_dir: Option<&std::ffi::OsStr>,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let target_w: Vec<u16> = target.encode_wide().chain(std::iter::once(0)).collect();
    let verb_w: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let dir_w = working_dir.map(|dir| {
        dir.encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    });
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // We run on a background worker. NOASYNC makes the Shell finish any
        // DDE/association hand-off before these UTF-16 buffers go out of scope;
        // it does not wait for the launched application to exit.
        fMask: SEE_MASK_NOASYNC,
        lpVerb: PCWSTR::from_raw(verb_w.as_ptr()),
        lpFile: PCWSTR::from_raw(target_w.as_ptr()),
        lpDirectory: dir_w
            .as_ref()
            .map_or(PCWSTR::null(), |dir| PCWSTR::from_raw(dir.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }
        .map_err(|e| std::io::Error::other(format!("Windows open failed: {e}")))
}

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
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GHND};
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
        // Ownership contract: on SUCCESS the system owns the HGLOBAL
        // (we must not free); on FAILURE ownership stays with us and
        // we must GlobalFree or the allocation leaks.
        if SetClipboardData(
            CF_UNICODETEXT.0 as u32,
            windows::Win32::Foundation::HANDLE(handle.0),
        )
        .is_err()
        {
            let _ = windows::Win32::Foundation::GlobalFree(handle);
        }
    }
}

#[cfg(not(windows))]
pub fn copy_to_clipboard(_text: &str) {}

/// Filesystem path of the running application, suitable for pasting
/// into a file picker. Returns the executable path. `None` if it
/// can't be determined.
pub fn app_bundle_path() -> Option<String> {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Open Explorer with `path` selected. Uses an absolute PIDL for the parent and
/// a relative child PIDL—the identity-preserving API Explorer itself exposes.
/// This avoids `explorer /select,<path>` command-line parsing, which silently
/// opened Documents for some valid names containing spaces / `#` / `!`.
#[cfg(windows)]
pub fn reveal_in_finder(path: &std::path::Path) {
    let _ = reveal_in_explorer(path);
}

#[cfg(not(windows))]
pub fn reveal_in_finder(_path: &std::path::Path) {}

#[cfg(windows)]
fn reveal_in_explorer(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        ILClone, ILFindLastID, ILFree, ILRemoveLastID, SHOpenFolderAndSelectItems,
        SHParseDisplayName,
    };

    struct Pidl(*mut ITEMIDLIST);
    impl Drop for Pidl {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { ILFree(Some(self.0.cast_const())) };
            }
        }
    }

    let cleaned = strip_verbatim(path);
    let path_w: Vec<u16> = cleaned
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        // Background executors may already be MTA. RPC_E_CHANGED_MODE means COM
        // is initialized in that apartment and the Shell call can proceed; only
        // balance CoUninitialize when this call actually initialized/incremented
        // COM for the thread.
        let co = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let we_initialized = co.is_ok();
        let result = (|| -> std::io::Result<()> {
            let mut absolute = std::ptr::null_mut();
            SHParseDisplayName(
                PCWSTR::from_raw(path_w.as_ptr()),
                None,
                &mut absolute,
                0,
                None,
            )
            .map_err(|e| std::io::Error::other(format!("parse Shell item: {e}")))?;
            if absolute.is_null() {
                return Err(std::io::Error::other("Shell returned an empty item id"));
            }
            let parent = Pidl(absolute);

            let child = Pidl(ILClone(ILFindLastID(parent.0)));
            if child.0.is_null() {
                return Err(std::io::Error::other("could not clone child item id"));
            }
            if !ILRemoveLastID(Some(parent.0)).as_bool() {
                return Err(std::io::Error::other("Shell item has no revealable parent"));
            }

            let children = [child.0.cast_const()];
            SHOpenFolderAndSelectItems(parent.0.cast_const(), Some(&children), 0)
                .map_err(|e| std::io::Error::other(format!("reveal in Explorer: {e}")))
        })();
        if we_initialized {
            CoUninitialize();
        }
        result
    }
}

/// Open a terminal at `path` (a directory) with the default spec:
/// Windows Terminal, `wt.exe -d <dir>`. See [`open_terminal_with`].
pub fn open_terminal(path: &std::path::Path) {
    open_terminal_with(path, &ferail_core::terminal::TerminalSpec::default());
}

/// Open a terminal at `path` (a directory) per the user's terminal
/// preferences (docs/features/CONTEXT_MENU.md).
///
/// Standard mode spawns the configured program (default: Windows
/// Terminal, `wt.exe -d <dir>`) with the resolved params; when the
/// params never mention `{dir}` the child inherits `current_dir(path)`
/// instead. With no custom terminal and no `wt.exe` on the machine, a
/// classic `cmd.exe` console at `path` is the fallback.
///
/// Admin mode launches the same program + params elevated via
/// `ShellExecuteExW` verb `"runas"` — the UAC prompt. Blocks until UAC
/// resolves (`SEE_MASK_NOASYNC`); callers run this on a worker (Prime
/// Directive). A dismissed prompt is treated like any failed launcher:
/// fire-and-forget.
#[cfg(windows)]
pub fn open_terminal_with(path: &std::path::Path, spec: &ferail_core::terminal::TerminalSpec) {
    // `wt.exe -d` (and cmd) mis-parse the `\\?\` verbatim form; hand
    // every child the plain path.
    let cleaned = strip_verbatim(path);
    let dir = cleaned.display().to_string();
    let (mut args, had_dir) = spec.resolved_args(&dir);
    let custom = spec.program();
    let program = custom.unwrap_or("wt.exe").to_string();
    // Default args for the default terminal: a new tab whose working
    // directory is `path` (wt ignores its own cwd by default).
    if custom.is_none() && args.is_empty() {
        args = vec!["-d".into(), dir.clone()];
    }

    if spec.admin() {
        shell_execute_runas(&program, &args, &cleaned);
        return;
    }

    let mut cmd = std::process::Command::new(&program);
    cmd.args(&args);
    if !had_dir {
        cmd.current_dir(&cleaned);
    }
    if cmd.spawn().is_ok() {
        return;
    }
    if custom.is_none() {
        let _ = std::process::Command::new("cmd.exe")
            .current_dir(&cleaned)
            .spawn();
    }
}

#[cfg(not(windows))]
pub fn open_terminal_with(_path: &std::path::Path, _spec: &ferail_core::terminal::TerminalSpec) {}

/// Launch `program` elevated (UAC) with `args`, working directory `dir`.
/// Best-effort: any failure — including the user dismissing the prompt —
/// is swallowed, matching the other launchers here.
#[cfg(windows)]
fn shell_execute_runas(program: &str, args: &[String], dir: &std::path::Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(Some(0))
            .collect()
    }
    let mut params = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            params.push(' ');
        }
        elevation::append_quoted(a, &mut params);
    }
    // These buffers must outlive the ShellExecuteExW call.
    let file_w = wide(program);
    let params_w = wide(&params);
    let verb_w = wide("runas");
    let dir_w = wide(&dir.display().to_string());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // NOASYNC: fully resolve before returning (we're on a worker).
        fMask: SEE_MASK_NOASYNC,
        lpVerb: PCWSTR::from_raw(verb_w.as_ptr()),
        lpFile: PCWSTR::from_raw(file_w.as_ptr()),
        lpParameters: PCWSTR::from_raw(params_w.as_ptr()),
        lpDirectory: PCWSTR::from_raw(dir_w.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    let _ = unsafe { ShellExecuteExW(&mut info) };
}

// =============================================================
// File operations
// =============================================================

/// Unmount and eject the volume mounted at `path` (a drive root like `E:\`).
/// Opens the volume device `\\.\E:`, flushes + locks it best-effort, dismounts
/// it (`FSCTL_DISMOUNT_VOLUME`), re-allows media removal, then ejects
/// (`IOCTL_STORAGE_EJECT_MEDIA` — opens the tray for optical media, powers down
/// removable media). Succeeds if either the dismount or the eject takes; a
/// hard failure (e.g. files still open) is surfaced to the host as a toast.
#[cfg(windows)]
pub fn eject_volume(path: &std::path::Path) -> Result<(), String> {
    eject_volume_inner(path, true)
}

/// Unmount every volume in `volume_paths` (drive roots on one physical
/// device), then eject/power down the device — Finder's "Eject All".
/// All volumes are dismounted **before** the media eject: ejecting
/// mid-loop could yank partitions that are still mounted. If any
/// dismount fails the media eject is skipped and the first error is
/// returned (already-dismounted siblings stay dismounted, like Finder).
#[cfg(windows)]
pub fn eject_device(volume_paths: &[&std::path::Path]) -> Result<(), String> {
    let Some((last, rest)) = volume_paths.split_last() else {
        return Err("eject: no volumes given".into());
    };
    for path in rest {
        eject_volume_inner(path, false)?;
    }
    eject_volume_inner(last, true)
}

#[cfg(not(windows))]
pub fn eject_device(_volume_paths: &[&std::path::Path]) -> Result<(), String> {
    Err("eject is not implemented on this OS".into())
}

/// Dismount one volume; with `eject_media` also allow removal + eject
/// the backing device (tray open / power down) afterwards.
#[cfg(windows)]
fn eject_volume_inner(path: &std::path::Path, eject_media: bool) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::{
        FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, IOCTL_STORAGE_EJECT_MEDIA,
        IOCTL_STORAGE_MEDIA_REMOVAL, PREVENT_MEDIA_REMOVAL,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    // GENERIC_READ | GENERIC_WRITE (kept as a literal to avoid the access-right
    // newtype churn between `windows` crate versions).
    const GENERIC_READ_WRITE: u32 = 0x8000_0000 | 0x4000_0000;

    let cleaned = strip_verbatim(path);
    let s = cleaned.to_string_lossy();
    // A UNC path (`\\server\share`, incl. the `\\?\UNC\…` verbatim form)
    // names a network location, not a local volume — refuse it outright.
    // (Before strip_verbatim, `\\?\UNC\server\…` trimmed to `UNC\…` and its
    // leading `U` parsed as drive `U:`, risking a dismount of an unrelated
    // real volume.)
    if s.starts_with(r"\\") {
        return Err(format!("eject: network (UNC) paths cannot be ejected: {s}"));
    }
    // Require the `X:` shape so an arbitrary relative path can't donate its
    // first letter as a drive.
    let mut chars = s.chars();
    let drive = match (chars.next(), chars.next()) {
        (Some(d), Some(':')) if d.is_ascii_alphabetic() => d,
        _ => return Err(format!("eject: not a drive path: {s}")),
    };
    let device = format!(r"\\.\{}:", drive.to_ascii_uppercase());
    let wide: Vec<u16> = device.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let handle = CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .map_err(|e| format!("open volume {device}: {e}"))?;

        let mut returned: u32 = 0;
        let mut ioctl = |code: u32, inbuf: Option<*const core::ffi::c_void>, insize: u32| {
            DeviceIoControl(
                handle,
                code,
                inbuf,
                insize,
                None,
                0,
                Some(&mut returned),
                None,
            )
        };

        // Best-effort lock (fails if files are open — dismount still tried).
        let _ = ioctl(FSCTL_LOCK_VOLUME, None, 0);
        let dismounted = ioctl(FSCTL_DISMOUNT_VOLUME, None, 0);

        if !eject_media {
            // Sibling pass of an eject-all: dismount only, the device
            // eject happens once after every volume is off it.
            let _ = CloseHandle(handle);
            if dismounted.is_err() {
                return Err(format!(
                    "could not dismount {device} (files may still be open)"
                ));
            }
            return Ok(());
        }

        // Allow the media to be removed, then eject.
        let mut allow = PREVENT_MEDIA_REMOVAL {
            PreventMediaRemoval: false.into(),
        };
        let _ = ioctl(
            IOCTL_STORAGE_MEDIA_REMOVAL,
            Some(&mut allow as *mut _ as *const core::ffi::c_void),
            std::mem::size_of::<PREVENT_MEDIA_REMOVAL>() as u32,
        );
        let ejected = ioctl(IOCTL_STORAGE_EJECT_MEDIA, None, 0);

        let _ = CloseHandle(handle);

        if dismounted.is_err() && ejected.is_err() {
            return Err(format!(
                "could not dismount or eject {device} (files may still be open)"
            ));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn eject_volume(_path: &std::path::Path) -> Result<(), String> {
    Err("eject is not implemented on this OS".into())
}

/// Names of processes holding files open on the volume at `path` — the
/// "why won't it eject" answer for a failed eject. Not implemented on
/// Windows yet (the honest source is the Restart Manager, which wants a
/// file list, not a volume; NtQuerySystemInformation handle walks need
/// admin). Callers must treat empty as "unknown", not "nothing".
pub fn volume_busy_processes(_path: &std::path::Path) -> Vec<ferail_core::BusyApp> {
    Vec::new()
}

/// Bring the app owning `pid` to the foreground. Not implemented on
/// Windows (would need an EnumWindows pid→HWND walk + SetForegroundWindow,
/// and there is no busy-process list to click yet — see above); callers
/// treat `false` as a no-op.
pub fn activate_app(_pid: i32) -> bool {
    false
}

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
///
/// Symlinks (and NTFS junctions, which std reports as symlinks) are
/// SKIPPED, for two reasons: (a) `fs::copy` on a symlink-to-dir
/// errors out, which used to fail the whole duplicate as soon as a
/// folder contained one; (b) following links invites cycles
/// (`mklink /D loop ..`) that would recurse forever. Explorer's own
/// folder-copy behavior for real symlinks is inconsistent across
/// versions; skipping is the conservative, never-hangs choice.
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        // DirEntry::file_type does NOT follow symlinks — is_symlink
        // is reliable here without an extra stat.
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Write a `.lnk` shortcut to `target` inside `dir`, picking an unused
/// `<stem>.lnk` / `<stem> (N).lnk` name. The canonical `IShellLink` /
/// `IPersistFile` COM dance: create the shell-link object, set its target,
/// persist. Returns the shortcut path. Shared by [`make_alias`] (drops next
/// to the target) and [`make_alias_in`] (drops into a destination folder).
#[cfg(windows)]
fn write_shell_link(
    target: &std::path::Path,
    dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{Interface, PCWSTR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    let stem = target
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "make_alias: target has no valid file stem".to_string())?;

    // Pick an unused .lnk path in `dir`.
    let mut shortcut: std::path::PathBuf = dir.join(format!("{stem}.lnk"));
    if shortcut.exists() {
        for n in 2..=99 {
            let candidate = dir.join(format!("{stem} ({n}).lnk"));
            if !candidate.exists() {
                shortcut = candidate;
                break;
            }
        }
    }
    if shortcut.exists() {
        return Err("make_alias: exhausted shortcut name slots".into());
    }

    // Store the .lnk target in plain form — a `\\?\` verbatim target baked
    // into the shortcut leaves Explorer unable to resolve it.
    let target_clean = strip_verbatim(target);
    let target_wide: Vec<u16> = target_clean
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let shortcut_wide: Vec<u16> = shortcut
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let co_hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let we_initialized = co_hr.is_ok();

        let result: Result<(), String> = (|| {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| e.to_string())?;
            link.SetPath(PCWSTR::from_raw(target_wide.as_ptr()))
                .map_err(|e| e.to_string())?;
            let persist: IPersistFile = link.cast().map_err(|e| e.to_string())?;
            persist
                .Save(PCWSTR::from_raw(shortcut_wide.as_ptr()), true)
                .map_err(|e| e.to_string())?;
            Ok(())
        })();

        if we_initialized {
            CoUninitialize();
        }
        result.map(|_| shortcut)
    }
}

/// Make a `.lnk` shortcut next to `target` pointing at it (the macOS
/// "Make Alias" equivalent). Drops `target.lnk` (or `target (N).lnk`) in the
/// target's own folder.
#[cfg(windows)]
pub fn make_alias(target: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let parent = target
        .parent()
        .ok_or_else(|| "make_alias: target has no parent".to_string())?;
    write_shell_link(target, parent)
}

#[cfg(not(windows))]
pub fn make_alias(_target: &std::path::Path) -> Result<std::path::PathBuf, String> {
    Err("make_alias: not implemented on this OS".into())
}

/// Make a `.lnk` shortcut to `target` inside `dest_dir` — the Windows form of
/// the Cmd+Option alias-drop (drag a file into a folder while making an alias
/// instead of moving). Drops `<stem>.lnk` into `dest_dir`.
#[cfg(windows)]
pub fn make_alias_in(
    target: &std::path::Path,
    dest_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    write_shell_link(target, dest_dir)
}

#[cfg(not(windows))]
pub fn make_alias_in(
    _target: &std::path::Path,
    _dest_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    Err("make_alias_in: not implemented on this OS".into())
}

/// Compress `targets` into a `.zip` written next to the first
/// target's parent. Naming matches Explorer's "Send to → Compressed
/// (zipped) folder": `<basename>.zip` for a single source,
/// `Archive.zip` for multiple. Picks the next free `(N)` suffix if
/// the chosen name is taken (capped at 99).
///
/// Compression: deflate at the `zip` crate's default level. Pure
/// Rust — no shell-out to `tar.exe`. Caller should invoke this on a
/// worker thread; large archives can take seconds.
pub fn compress_paths(targets: &[&std::path::Path]) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    if targets.is_empty() {
        return Err("compress_paths: no targets".into());
    }

    let parent = targets[0]
        .parent()
        .ok_or_else(|| "compress_paths: first target has no parent".to_string())?;
    let base = if targets.len() == 1 {
        targets[0]
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| "Archive".to_string())
    } else {
        "Archive".to_string()
    };

    // Choose unused zip path: <base>.zip, then <base> (2).zip, ...
    let mut out_path: std::path::PathBuf = parent.join(format!("{base}.zip"));
    if out_path.exists() {
        let mut chosen = None;
        for n in 2..=99 {
            let candidate = parent.join(format!("{base} ({n}).zip"));
            if !candidate.exists() {
                chosen = Some(candidate);
                break;
            }
        }
        out_path = chosen.ok_or_else(|| "compress_paths: name slots exhausted".to_string())?;
    }

    let file = std::fs::File::create(&out_path).map_err(|e| format!("create zip: {e}"))?;
    let mut writer = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for src in targets {
        let arc_base = src
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("compress_paths: invalid filename for {}", src.display()))?;
        if src.is_dir() {
            // Walk recursively. Each entry's arc path is
            // `<arc_base>/<relative subpath>`.
            walk_into_zip(&mut writer, src, arc_base, opts)?;
        } else {
            writer
                .start_file(arc_base, opts)
                .map_err(|e| format!("zip start_file: {e}"))?;
            let bytes = std::fs::read(src).map_err(|e| format!("read {}: {e}", src.display()))?;
            writer
                .write_all(&bytes)
                .map_err(|e| format!("zip write: {e}"))?;
        }
    }
    writer.finish().map_err(|e| format!("zip finish: {e}"))?;
    Ok(out_path)
}

fn walk_into_zip(
    writer: &mut zip::ZipWriter<std::fs::File>,
    src_dir: &std::path::Path,
    arc_prefix: &str,
    opts: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    use std::io::Write;
    let mut stack: Vec<(std::path::PathBuf, String)> =
        vec![(src_dir.to_path_buf(), arc_prefix.to_string())];
    while let Some((dir, arc)) = stack.pop() {
        writer
            .add_directory(format!("{arc}/"), opts)
            .map_err(|e| format!("zip add_directory: {e}"))?;
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| format!("compress_paths: invalid filename in {}", dir.display()))?;
            let arc_path = format!("{arc}/{name}");
            let ft = entry.file_type().map_err(|e| format!("file_type: {e}"))?;
            // Skip symlinks/junctions — same rationale as
            // copy_dir_recursive: a link cycle would otherwise grow
            // the walk stack forever, and archiving link targets
            // through their parent dir double-stores content.
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push((path, arc_path));
            } else if ft.is_file() {
                writer
                    .start_file(&arc_path, opts)
                    .map_err(|e| format!("zip start_file: {e}"))?;
                let bytes =
                    std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
                writer
                    .write_all(&bytes)
                    .map_err(|e| format!("zip write: {e}"))?;
            }
        }
    }
    Ok(())
}

// =============================================================
// Quick Look — N/A on Windows (no system Quick Look)
// =============================================================

/// macOS Quick Look has no Windows equivalent. The eventual port will
/// either drop the feature or build an in-process preview window.
pub fn show_quick_look(_paths: &[&std::path::Path]) -> Result<(), String> {
    Err("show_quick_look: not available on Windows".into())
}

/// Generate a thumbnail at up to `size_px` on the longest side.
/// Returns `(RGBA bytes, width, height)` on success; `None` on any
/// failure (path not found, no thumbnail provider, GDI error, etc.).
///
/// Pipeline: `SHCreateItemFromParsingName` → `IShellItemImageFactory`
/// → `GetImage(SIZE, SIIGBF_RESIZETOFIT)` → `HBITMAP` → `GetDIBits`
/// → BGRA bytes → swap to RGBA. The shell's built-in thumbnail
/// pipeline taps registered thumbnail providers (image files,
/// PDFs via the Microsoft PDF preview handler, etc.) and falls
/// back to large file-type icons for the rest.
///
/// MUST be called from a worker thread — COM is initialized per-
/// call with `COINIT_APARTMENTTHREADED`. Calling from the UI thread
/// blocks paint while the shell extension generates the bitmap;
/// per the prime directive in `docs/ARCHITECTURE.md`, the caller
/// is responsible for scheduling this off the paint path.
#[cfg(windows)]
pub fn fetch_quick_look_thumbnail(
    path: &std::path::Path,
    size_px: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{DeleteObject, GetObjectW, DIBSECTION, HBITMAP};
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_RESIZETOFIT,
        SIIGBF_THUMBNAILONLY,
    };

    // `SHCreateItemFromParsingName` rejects the `\\?\` verbatim prefix, so
    // parse the plain form (the preview-handler fallback below gets the same
    // cleaned path — its `IInitializeWithFile` has the same restriction).
    let cleaned = strip_verbatim(path);
    // Convert the path to a null-terminated UTF-16 string. Holding
    // it for the duration of the COM call so the PCWSTR remains
    // valid.
    let mut wide: Vec<u16> = cleaned.as_os_str().encode_wide().collect();
    wide.push(0);

    unsafe {
        // COM init for this thread. If COM is already initialized
        // (e.g. by another worker), CoInitializeEx returns
        // S_FALSE / RPC_E_CHANGED_MODE — both are non-fatal, we
        // still proceed and skip CoUninitialize below.
        let co_hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let we_initialized = co_hr.is_ok();

        // Run the actual extraction in a closure so the cleanup
        // path is single-exit (CoUninitialize + HBITMAP delete on
        // any return path). When FERAIL_THUMB_DEBUG=1 each step
        // logs to stderr so we can isolate failures.
        let debug = std::env::var("FERAIL_THUMB_DEBUG").is_ok();
        let result = (|| -> Option<(Vec<u8>, u32, u32)> {
            // Create IShellItem from the path string.
            let factory: IShellItemImageFactory =
                match SHCreateItemFromParsingName(PCWSTR::from_raw(wide.as_ptr()), None) {
                    Ok(f) => f,
                    Err(e) => {
                        if debug {
                            eprintln!("SHCreateItemFromParsingName failed: {e:?}");
                        }
                        return None;
                    }
                };
            if debug {
                eprintln!("SHCreateItemFromParsingName ok");
            }

            // Ask for the thumbnail.
            let size = SIZE {
                cx: size_px as i32,
                cy: size_px as i32,
            };
            // Fallback chain:
            //   1. SIIGBF_THUMBNAILONLY — only real provider-rendered
            //      thumbnails (PNG, PPTX, images with EXIF previews,
            //      etc.). Fails when only an icon would be returned.
            //   2. IPreviewHandler — for files with no thumbnail
            //      provider but with a registered preview handler
            //      (Word/Excel docs, RTF, …). Renders the doc's
            //      content into an off-screen window and screen-grabs.
            //   3. SIIGBF_RESIZETOFIT — generic large file-type icon.
            // `is_icon_fallback` tracks which sub-API produced the
            // bitmap so we can orient correctly. Empirically:
            //   - THUMBNAILONLY thumbnails arrive top-down regardless
            //     of biHeight sign (PowerPoint, Excel, image
            //     thumbnails).
            //   - Generic file-type icons returned by RESIZETOFIT
            //     when no thumbnail provider exists arrive bottom-up
            //     (Win32 ICON resource convention) — those need a
            //     vertical flip.
            let mut is_icon_fallback = false;
            let hbitmap: HBITMAP =
                match factory.GetImage(size, SIIGBF_THUMBNAILONLY | SIIGBF_RESIZETOFIT) {
                    Ok(h) => {
                        if debug {
                            eprintln!("THUMBNAILONLY ok, hbitmap={:?}", h);
                        }
                        h
                    }
                    Err(e) => {
                        if debug {
                            eprintln!("THUMBNAILONLY failed: {e:?} — trying preview handler");
                        }
                        if let Some(rgba) = preview_handler::try_capture(&cleaned, size_px) {
                            if debug {
                                eprintln!("preview handler ok");
                            }
                            return Some(rgba);
                        }
                        if debug {
                            eprintln!("preview handler failed — falling back to icon");
                        }
                        is_icon_fallback = true;
                        match factory.GetImage(size, SIIGBF_RESIZETOFIT) {
                            Ok(h) => h,
                            Err(e) => {
                                if debug {
                                    eprintln!("icon fallback failed: {e:?}");
                                }
                                return None;
                            }
                        }
                    }
                };

            // Pull a DIBSECTION view of the bitmap so we get direct
            // access to its in-memory pixel buffer. `IShellItemImage-
            // Factory::GetImage` always returns 32bpp BGRA DIB sections
            // — using GetDIBits with BI_RGB was an earlier approach
            // but it strips the alpha channel (the 4th byte ends up
            // undefined / zero), which made transparent icon
            // backgrounds render as opaque black. Reading the section
            // bits directly preserves the original premultiplied-alpha
            // BGRA values.
            let mut ds = DIBSECTION::default();
            let nb = GetObjectW(
                hbitmap,
                std::mem::size_of::<DIBSECTION>() as i32,
                Some(&mut ds as *mut _ as *mut _),
            );
            if nb == 0 || ds.dsBm.bmBits.is_null() {
                if debug {
                    eprintln!(
                        "GetObjectW(DIBSECTION) returned {} bits={:?}",
                        nb, ds.dsBm.bmBits
                    );
                }
                let _ = DeleteObject(hbitmap);
                return None;
            }
            let w = ds.dsBm.bmWidth as u32;
            let h_signed = ds.dsBmih.biHeight;
            let h = h_signed.unsigned_abs();
            let bpp = ds.dsBm.bmBitsPixel as usize;
            let stride = ds.dsBm.bmWidthBytes as usize;
            if debug {
                eprintln!(
                    "DIBSECTION: w={} h={} bpp={} stride={} biHeight={}",
                    w, h, bpp, stride, h_signed
                );
            }
            if bpp != 32 {
                if debug {
                    eprintln!("Unsupported bpp: {}", bpp);
                }
                let _ = DeleteObject(hbitmap);
                return None;
            }

            // Empirically the THUMBNAILONLY thumbnail HBITMAPs come
            // back top-down (so we walk in source order), but the
            // RESIZETOFIT icon-fallback HBITMAPs come back bottom-up
            // — see is_icon_fallback above.
            let src = ds.dsBm.bmBits as *const u8;
            let mut pixels: Vec<u8> = vec![0u8; (w as usize) * (h as usize) * 4];
            let row_bytes = (w as usize) * 4;
            for y in 0..(h as usize) {
                let src_row = if is_icon_fallback {
                    (h as usize) - 1 - y
                } else {
                    y
                };
                let src_ptr = src.add(src_row * stride);
                let dst_off = y * row_bytes;
                std::ptr::copy_nonoverlapping(src_ptr, pixels.as_mut_ptr().add(dst_off), row_bytes);
            }
            let _ = DeleteObject(hbitmap);

            // The DIB carries pre-multiplied-alpha BGRA. Caller
            // (preview.rs) expects RGBA, then swaps back to BGRA for
            // gpui rendering — net result the renderer sees BGRA,
            // which is what its `RgbaImage` shim wants. So we swap
            // B<->R here once.
            //
            // Some thumbnail providers return BGRA with alpha=0
            // everywhere even for opaque images (e.g. JPEGs read
            // through the legacy thumbnail path). Detect that case
            // and treat the image as fully opaque so the preview
            // isn't invisible.
            let all_alpha_zero = pixels.chunks_exact(4).all(|px| px[3] == 0);
            for px in pixels.chunks_exact_mut(4) {
                px.swap(0, 2);
                if all_alpha_zero {
                    px[3] = 0xFF;
                }
            }
            if debug && all_alpha_zero {
                eprintln!("all alpha bytes were 0 — forcing opaque");
            }

            Some((pixels, w, h))
        })();

        if we_initialized {
            CoUninitialize();
        }
        result
    }
}

#[cfg(not(windows))]
pub fn fetch_quick_look_thumbnail(
    _path: &std::path::Path,
    _size_px: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    None
}

// =============================================================
// Get Info — shell facts (the NSURL-resource-values equivalent)
// =============================================================

/// Shell-sourced Get Info facts. On Windows the meaningful one is the
/// localized **type description** (`SHGetFileInfoW` + `SHGFI_TYPENAME`) — the
/// "Type" field the Properties dialog shows, e.g. "Markdown Source File" or
/// "File folder". There is no per-file UTI, "date added", or per-file
/// hide-extension flag, so those stay `None`; the `.lnk` extension marks a
/// shortcut (Windows' alias). Mirrors `ferail_shell_mac::read_shell_info`.
#[cfg(windows)]
pub fn read_shell_info(path: &std::path::Path) -> ShellInfo {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{
        SHGetFileInfoW, SHFILEINFOW, SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES,
    };

    let info = ShellInfo {
        is_alias: Some(
            path.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("lnk")),
        ),
        ..Default::default()
    };

    // Shell APIs reject the `\\?\` extended-length prefix the file list uses
    // internally; strip it so SHGetFileInfoW resolves.
    let cleaned = strip_verbatim(path);
    let display = cleaned.to_string_lossy();

    // SHGFI_USEFILEATTRIBUTES makes SHGetFileInfoW derive the type name from
    // the path *string* + the attributes we assert, never touching disk —
    // without it, a dead UNC path blocked this worker for the full SMB
    // timeout. The signature carries no is_dir hint, so pick the attribute
    // from the path shape:
    //   - trailing separator → directory ("File folder"),
    //   - extension present  → ordinary file (type name from the extension),
    //   - extensionless, no trailing separator → ambiguous. Skip the lookup
    //     (kind stays None): SHGetFileInfoW would only answer the generic
    //     "File"/"File folder" here, and guessing wrong would mislabel
    //     folders as files (or LICENSE-style files as folders) in Get Info.
    //     The caller's fallback chain (magic sniff, then its own is_dir
    //     classification) names these correctly.
    let attrs = if display.ends_with('\\') || display.ends_with('/') {
        FILE_ATTRIBUTE_DIRECTORY
    } else if cleaned.extension().is_some() {
        FILE_ATTRIBUTE_NORMAL
    } else {
        return info;
    };
    let mut info = info;

    let wide: Vec<u16> = display.encode_utf16().chain(std::iter::once(0)).collect();
    let mut shfi = SHFILEINFOW::default();
    unsafe {
        // Same per-call COM pattern as `fetch_quick_look_thumbnail` /
        // `open_with_candidates`: S_FALSE (already initialized on this
        // thread) is a success and still needs the balancing
        // CoUninitialize; RPC_E_CHANGED_MODE is a failure HRESULT, so
        // `is_ok()` is false and we skip the uninitialize.
        let co_hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let we_initialized = co_hr.is_ok();

        let res = SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            attrs,
            Some(&mut shfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
        );
        if res != 0 {
            let len = shfi
                .szTypeName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(shfi.szTypeName.len());
            let kind = String::from_utf16_lossy(&shfi.szTypeName[..len]);
            if !kind.is_empty() {
                info.kind = Some(kind);
            }
        }

        if we_initialized {
            CoUninitialize();
        }
    }
    info
}

#[cfg(not(windows))]
pub fn read_shell_info(_path: &std::path::Path) -> ShellInfo {
    ShellInfo::default()
}

/// Set Finder's per-file "Hide extension" flag. Windows has no per-file
/// equivalent (Explorer hides known extensions globally), so this is
/// unsupported. Mirrors `ferail_shell_mac::set_hidden_extension`.
pub fn set_hidden_extension(_path: &std::path::Path, _hide: bool) -> Result<(), String> {
    Err("hiding the extension per-file is macOS-only".into())
}

// =============================================================
// Finder Tags — no Windows equivalent
// =============================================================

/// Read canonical Finder colour tags. Windows has no system-wide tag
/// store; either drop the feature on Windows or back it with
/// `ferail-meta` (SQLite) only.
pub fn read_canonical_tags(_path: &std::path::Path) -> Vec<ferail_core::commands::TagColor> {
    Vec::new()
}

/// Toggle a Finder colour tag. See note above on `read_canonical_tags`.
pub fn toggle_tag(
    _path: &std::path::Path,
    _color: ferail_core::commands::TagColor,
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

/// Enumerate apps Windows would offer for "Open With" on `path`.
/// Uses `SHAssocEnumHandlers` with `ASSOC_FILTER_RECOMMENDED` so we
/// only return the explicitly-recommended set (avoids the very long
/// "more apps" tail). Caps the list at 12 to match the shell-mac
/// catalogue's slot count.
#[cfg(windows)]
pub fn open_with_candidates(path: &std::path::Path) -> Vec<OpenWithCandidate> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{SHAssocEnumHandlers, ASSOC_FILTER_RECOMMENDED};

    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    let ext_with_dot = format!(".{}", ext);
    let wide: Vec<u16> = ext_with_dot
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut out: Vec<OpenWithCandidate> = Vec::new();
    unsafe {
        let co_hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let we_initialized = co_hr.is_ok();

        if let Ok(enumerator) =
            SHAssocEnumHandlers(PCWSTR::from_raw(wide.as_ptr()), ASSOC_FILTER_RECOMMENDED)
        {
            loop {
                let mut handler = [None; 1];
                let mut fetched: u32 = 0;
                if enumerator.Next(&mut handler, Some(&mut fetched)).is_err() {
                    break;
                }
                if fetched == 0 {
                    break;
                }
                let Some(h) = handler[0].as_ref() else {
                    break;
                };
                let (path_str, name_pwstr_raw) = match h.GetName() {
                    Ok(p) if !p.is_null() => {
                        (p.to_string().unwrap_or_default(), p.as_ptr() as *const _)
                    }
                    _ => (String::new(), std::ptr::null::<core::ffi::c_void>()),
                };
                let (display, ui_pwstr_raw) = match h.GetUIName() {
                    Ok(p) if !p.is_null() => {
                        (p.to_string().unwrap_or_default(), p.as_ptr() as *const _)
                    }
                    _ => (String::new(), std::ptr::null::<core::ffi::c_void>()),
                };
                // Free the COM-allocated strings.
                if !name_pwstr_raw.is_null() {
                    windows::Win32::System::Com::CoTaskMemFree(Some(name_pwstr_raw));
                }
                if !ui_pwstr_raw.is_null() {
                    windows::Win32::System::Com::CoTaskMemFree(Some(ui_pwstr_raw));
                }
                if display.is_empty() && path_str.is_empty() {
                    continue;
                }
                let name = if display.is_empty() {
                    path_str.clone()
                } else {
                    display
                };
                let is_default = out.is_empty();
                out.push(OpenWithCandidate {
                    name,
                    path: std::path::PathBuf::from(path_str),
                    is_default,
                });
                if out.len() >= 12 {
                    break;
                }
            }
        }

        if we_initialized {
            CoUninitialize();
        }
    }

    let _ = wide;
    out
}

#[cfg(not(windows))]
pub fn open_with_candidates(_path: &std::path::Path) -> Vec<OpenWithCandidate> {
    Vec::new()
}

/// Open `target` with the app at `app_path`. Uses `std::process::Command`
/// rather than `ShellExecuteExW` — sufficient for invoking a normal
/// .exe with the file as its argument, no UAC elevation, no PATH
/// resolution magic.
#[cfg(windows)]
pub fn open_with_app(target: &std::path::Path, app_path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new(app_path)
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("open_with_app: {e}"))
}

#[cfg(not(windows))]
pub fn open_with_app(_target: &std::path::Path, _app_path: &std::path::Path) -> Result<(), String> {
    Err("open_with_app: not implemented on this OS".into())
}

/// Open every `target` with the app at `app_path`. One process per
/// file — the spawns don't wait, so the loop is cheap; the batch form
/// exists for parity with shell-mac's single-invocation `open -a`.
pub fn open_with_app_many(
    targets: &[std::path::PathBuf],
    app_path: &std::path::Path,
) -> Result<(), String> {
    let mut last_err = None;
    for target in targets {
        if let Err(e) = open_with_app(target, app_path) {
            last_err = Some(e);
        }
    }
    match last_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// =============================================================
// App icon / theme
// =============================================================

/// Set the running process's icon at runtime.
///
/// **Windows v1 is a no-op.** Windows apps conventionally attach an
/// `.ico` via the resource section / app manifest at build time —
/// that icon is already in place by the time this function would
/// ever fire. See `ferail-gpui/build.rs`, which uses `winresource`
/// to embed `resources/ferail.ico` into the .exe so Explorer, the
/// taskbar, Alt-Tab, and the title bar all pick it up automatically.
/// The runtime alternative (`SendMessage(WM_SETICON)` on every
/// top-level HWND) brings PNG decoding + `CreateIconIndirect` +
/// window enumeration for no user-visible gain over the manifest
/// path. If a real runtime-swap is ever needed (e.g. dynamic
/// per-window state badges), this is where it goes.
///
/// Returns `Ok` so callers don't surface a misleading error; the
/// PNG bytes are intentionally ignored.
pub fn set_app_icon_from_png_bytes(_png_bytes: &[u8]) -> SetIconResult {
    SetIconResult::Ok
}

/// Tell the Windows shell that this process is its own application
/// for taskbar grouping, jump-list, and pin-to-Start purposes. Without
/// this, Windows groups our window under whatever inherits the parent
/// console's AUMID — typically "Windows PowerShell" when launched from
/// a terminal, which is why the taskbar icon and label end up wrong.
///
/// Must be called before any UI is shown (the shell caches the AUMID
/// on the first window the process surfaces). One-shot, idempotent.
#[cfg(windows)]
pub fn set_app_user_model_id(id: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(PCWSTR::from_raw(wide.as_ptr()));
    }
}

#[cfg(not(windows))]
pub fn set_app_user_model_id(_id: &str) {}

/// `true` if Windows is in dark mode (per the "Apps" preference, not
/// the "Windows" preference, which matches what most native apps use).
/// Reads the DWORD at:
/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme`
/// which is `0` for dark, `1` for light. Missing key → defaults to
/// light (`false`). Matches the convention in Windows 10 1809+.
#[cfg(windows)]
pub fn system_is_dark() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

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

/// Force the app-wide native appearance to match the chosen theme.
/// Stub on Windows: the predecessor stack doesn't drive native chrome
/// this way yet; the mac shell owns the real implementation. Kept so
/// `platform_shell::set_app_appearance` resolves cross-platform.
pub fn set_app_appearance(_dark: bool) {}

/// Whether a "Show Desktop" affordance should appear. macOS resolves a
/// private CoreDock symbol for this; Windows has the Win+D shell verb but
/// the sidebar button is a macOS concept, so it's hidden here.
pub fn show_desktop_available() -> bool {
    false
}

/// Perform the "Show Desktop" reveal. No-op on Windows (the button is
/// hidden — see [`show_desktop_available`]); returns `false` to report it
/// did nothing.
pub fn show_desktop() -> bool {
    false
}

/// Raise every app window preserving z-order (macOS `arrangeInFront:`).
/// No Windows equivalent wired yet — `false` makes the caller fall back
/// to raising each window through gpui.
pub fn bring_all_windows_to_front() -> bool {
    false
}

/// Subscribe to system theme changes.
///
/// Spawns a dedicated worker thread that owns a message-only `HWND`
/// (`CreateWindowExW(HWND_MESSAGE, ...)`). Its WndProc filters
/// `WM_SETTINGCHANGE` for `lParam == "ImmersiveColorSet"` (the
/// Windows-internal signal that the user toggled the Personalize →
/// Apps dark/light setting) and on each match re-reads
/// [`system_is_dark`] and forwards the result via the callback.
///
/// # Thread contract — read before changing the callback
///
/// **The callback is invoked ON THE OBSERVER'S WORKER THREAD**, not
/// the UI thread (this differs from shell-mac, whose observer fires
/// on the main thread). It must not touch gpui entities, `Window`s,
/// or any main-thread-only state. The supported pattern is what
/// `main.rs` does today: write to a thread-safe cell
/// (`shell::set_system_theme_pending` — an atomic the Shell polls at
/// render). If a future caller needs more, marshal to the main
/// thread yourself; do not widen this callback's responsibilities.
///
/// The thread, window, and callback live for the lifetime of the
/// process. Each `start_system_theme_observer` call adds another
/// observer — callers should avoid invoking it repeatedly.
#[cfg(windows)]
pub fn start_system_theme_observer(callback: Box<dyn Fn(bool) + 'static + Send>) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassExW,
        SetWindowLongPtrW, TranslateMessage, GWLP_USERDATA, HMENU, MSG, WINDOW_EX_STYLE,
        WINDOW_STYLE, WM_SETTINGCHANGE, WNDCLASSEXW,
    };

    // HWND_MESSAGE = -3. Not exposed as a constant in windows-0.58;
    // use the raw value.
    const HWND_MESSAGE: isize = -3;

    // Thunk: WndProc reads the boxed callback out of GWLP_USERDATA
    // and invokes it on each ImmersiveColorSet change.
    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_SETTINGCHANGE && lparam.0 != 0 {
            // lParam is an LPCTSTR pointing at the setting name.
            let p = lparam.0 as *const u16;
            let mut len = 0usize;
            while len < 64 && *p.add(len) != 0 {
                len += 1;
            }
            let name = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            if name == "ImmersiveColorSet" {
                let user_data =
                    windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if user_data != 0 {
                    let cb_ptr = user_data as *const Box<dyn Fn(bool) + 'static + Send>;
                    let cb = &*cb_ptr;
                    cb(system_is_dark());
                }
            }
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    std::thread::Builder::new()
        .name("ferail-theme-observer".into())
        .spawn(move || unsafe {
            let class_name: Vec<u16> = "FerailThemeObserver\0".encode_utf16().collect();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                hInstance: HINSTANCE::default(),
                lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
                ..Default::default()
            };
            // Idempotent register — ignore "class already exists".
            let _ = RegisterClassExW(&wc);

            let hwnd = match CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR::from_raw(class_name.as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND(HWND_MESSAGE as *mut _),
                HMENU::default(),
                HINSTANCE::default(),
                None,
            ) {
                Ok(h) => h,
                Err(_) => return,
            };

            // Leak the callback into a stable heap pointer; tied to
            // the window lifetime (process-long).
            let cb_box: *mut Box<dyn Fn(bool) + 'static + Send> = Box::into_raw(Box::new(callback));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cb_box as isize);

            // Standard message pump. Returns when GetMessage gets
            // WM_QUIT — never, in this thread's lifetime.
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .ok();
}

#[cfg(not(windows))]
pub fn start_system_theme_observer(_callback: Box<dyn Fn(bool) + 'static + Send>) {}

/// Place `paths` on the clipboard as `CF_HDROP`, the format Explorer
/// (and our own paste path) reads for a file copy/cut. Layout is a
/// `DROPFILES` header immediately followed by a UTF-16 path list where
/// each path is null-terminated and the whole list ends with a second
/// null — packed into one moveable `HGLOBAL` and handed to
/// `SetClipboardData(CF_HDROP, ...)`. `GHND` zero-inits the block, so
/// only `pFiles` (offset to the list) and `fWide` (UTF-16 marker) need
/// setting. Best-effort and silent on failure, matching the mac
/// `NSPasteboard` contract. Empty input is a no-op. docs/features/FILE_OPS.md.
#[cfg(windows)]
pub fn clipboard_copy_file_urls(items: &[(&std::path::Path, bool)]) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GHND};
    use windows::Win32::System::Ole::CF_HDROP;
    use windows::Win32::UI::Shell::DROPFILES;

    if items.is_empty() {
        return false;
    }

    /// Pair every early return with `CloseClipboard` (mirrors
    /// `copy_to_clipboard`).
    struct CloseGuard;
    impl Drop for CloseGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }

    // Wide path list: each path null-terminated, then a final null to
    // close the double-null-terminated CF_HDROP list. The `is_dir`
    // hint is a mac-pasteboard need; CF_HDROP carries bare paths.
    // Verbatim-strip each entry — CF_HDROP paths carrying `\\?\` make
    // Explorer's paste silently fail.
    let mut list: Vec<u16> = Vec::new();
    for (p, _is_dir) in items {
        let cleaned = strip_verbatim(p);
        list.extend(cleaned.as_os_str().encode_wide());
        list.push(0);
    }
    list.push(0);

    let header = std::mem::size_of::<DROPFILES>();
    let bytes = header + list.len() * std::mem::size_of::<u16>();

    unsafe {
        if OpenClipboard(None).is_err() {
            return false;
        }
        let _guard = CloseGuard;
        let _ = EmptyClipboard();
        let Ok(handle) = GlobalAlloc(GHND, bytes) else {
            return false;
        };
        let base = GlobalLock(handle) as *mut u8;
        if base.is_null() {
            // Lock failure: nothing owns the allocation yet — free it
            // or it leaks.
            let _ = windows::Win32::Foundation::GlobalFree(handle);
            return false;
        }
        // DROPFILES sits at the front; the path list follows it. GHND
        // already zeroed pt/fNC, so leave those.
        let df = base as *mut DROPFILES;
        (*df).pFiles = header as u32;
        (*df).fWide = true.into();
        std::ptr::copy_nonoverlapping(list.as_ptr(), base.add(header) as *mut u16, list.len());
        let _ = GlobalUnlock(handle);
        // Same ownership contract as copy_to_clipboard: on success the
        // system owns the HGLOBAL; on failure we must free it.
        if SetClipboardData(CF_HDROP.0 as u32, HANDLE(handle.0)).is_err() {
            let _ = windows::Win32::Foundation::GlobalFree(handle);
            return false;
        }
        true
    }
}

#[cfg(not(windows))]
pub fn clipboard_copy_file_urls(_items: &[(&std::path::Path, bool)]) -> bool {
    false
}

/// Read a `CF_HDROP` file list off the clipboard (e.g. files a user
/// copied in Explorer, or via our own [`clipboard_copy_file_urls`]).
/// Walks the drop with `DragQueryFileW`: index `0xFFFFFFFF` gives the
/// count, then each index gives its path length and content. Returns
/// an empty vec when the clipboard holds no file drop. The handle is
/// owned by the clipboard, so it is neither freed nor `DragFinish`ed
/// (that is only for `WM_DROPFILES`).
#[cfg(windows)]
pub fn clipboard_read_file_urls() -> Vec<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Ole::CF_HDROP;
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

    struct CloseGuard;
    impl Drop for CloseGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }

    let mut out = Vec::new();
    unsafe {
        if OpenClipboard(None).is_err() {
            return out;
        }
        let _guard = CloseGuard;
        let Ok(handle) = GetClipboardData(CF_HDROP.0 as u32) else {
            return out;
        };
        if handle.0.is_null() {
            return out;
        }
        let hdrop = HDROP(handle.0);
        let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
        for i in 0..count {
            // None buffer → required length in chars, excluding null.
            let len = DragQueryFileW(hdrop, i, None);
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let copied = DragQueryFileW(hdrop, i, Some(buf.as_mut_slice()));
            if copied == 0 {
                continue;
            }
            buf.truncate(copied as usize);
            out.push(std::path::PathBuf::from(std::ffi::OsString::from_wide(
                &buf,
            )));
        }
    }
    out
}

#[cfg(not(windows))]
pub fn clipboard_read_file_urls() -> Vec<std::path::PathBuf> {
    Vec::new()
}

/// Subscribe to volume (drive-letter) mount/unmount.
///
/// Spawns a worker thread that owns a hidden window and forwards a
/// `WM_DEVICECHANGE` for `DBT_DEVICEARRIVAL` / `DBT_DEVICEREMOVECOMPLETE`
/// of device type `DBT_DEVTYP_VOLUME` to the callback. The gpui host
/// uses this to refresh the Volumes sidebar and re-evaluate Favorites
/// "Missing" state on mount/unmount.
///
/// # Why a top-level window, not the message-only one
///
/// [`start_system_theme_observer`] uses an `HWND_MESSAGE` window, but
/// that pattern does **not** work here: message-only windows are
/// excluded from broadcast messages, and drive-letter volume changes
/// arrive **only** as `WM_DEVICECHANGE` broadcasts to top-level
/// windows (the high-level `DBT_DEVTYP_VOLUME` type can't be obtained
/// via `RegisterDeviceNotification`, which only yields
/// `DBT_DEVTYP_DEVICEINTERFACE`). So this creates an ordinary hidden
/// top-level window (null parent, no `WS_VISIBLE`) which receives the
/// broadcasts without ever appearing on screen.
///
/// # Thread contract
///
/// Same as [`start_system_theme_observer`]: **the callback fires on
/// the observer's worker thread** (hence `Send`), not the UI thread.
/// It must not touch gpui entities — marshal to the main thread (the
/// gpui host posts through a channel). The thread, window, and
/// callback live for the process lifetime.
#[cfg(windows)]
pub fn start_volume_observer(callback: Box<dyn Fn() + 'static + Send>) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW,
        RegisterClassExW, SetWindowLongPtrW, TranslateMessage, GWLP_USERDATA, HMENU, MSG,
        WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSEXW,
    };

    // Raw values (winuser.h / dbt.h) — not all are typed constants in
    // windows-0.58, and we read the header field by offset to avoid
    // pulling the DEV_BROADCAST_HDR binding.
    const WM_DEVICECHANGE: u32 = 0x0219;
    const DBT_DEVICEARRIVAL: usize = 0x8000;
    const DBT_DEVICEREMOVECOMPLETE: usize = 0x8004;
    const DBT_DEVTYP_VOLUME: u32 = 0x0000_0002;

    // Thunk: on a volume arrive/remove broadcast, read the boxed
    // callback out of GWLP_USERDATA and fire it.
    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_DEVICECHANGE
            && (wparam.0 == DBT_DEVICEARRIVAL || wparam.0 == DBT_DEVICEREMOVECOMPLETE)
            && lparam.0 != 0
        {
            // lParam → DEV_BROADCAST_HDR; dbch_devicetype is the second
            // u32. Filter to volumes so raw device-interface arrivals
            // (USB enumeration, etc.) don't spam the callback.
            let devicetype = *((lparam.0 as *const u32).add(1));
            if devicetype == DBT_DEVTYP_VOLUME {
                let user_data = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if user_data != 0 {
                    let cb_ptr = user_data as *const Box<dyn Fn() + 'static + Send>;
                    (*cb_ptr)();
                }
            }
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    std::thread::Builder::new()
        .name("ferail-volume-observer".into())
        .spawn(move || unsafe {
            let class_name: Vec<u16> = "FerailVolumeObserver\0".encode_utf16().collect();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                hInstance: HINSTANCE::default(),
                lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
                ..Default::default()
            };
            // Idempotent register — ignore "class already exists".
            let _ = RegisterClassExW(&wc);

            // Null parent + WINDOW_STYLE(0) (WS_OVERLAPPED, no
            // WS_VISIBLE) → a hidden top-level window that still
            // receives WM_DEVICECHANGE broadcasts.
            let hwnd = match CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR::from_raw(class_name.as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND::default(),
                HMENU::default(),
                HINSTANCE::default(),
                None,
            ) {
                Ok(h) => h,
                Err(_) => return,
            };

            // Leak the callback into a stable heap pointer; tied to the
            // window lifetime (process-long).
            let cb_box: *mut Box<dyn Fn() + 'static + Send> = Box::into_raw(Box::new(callback));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cb_box as isize);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .ok();
}

#[cfg(not(windows))]
pub fn start_volume_observer(_callback: Box<dyn Fn() + 'static + Send>) {}

/// Begin observing system sleep / resume. Twin of
/// [`start_system_theme_observer`] / [`start_volume_observer`].
///
/// Spawns a worker thread owning a hidden top-level window and forwards
/// `WM_POWERBROADCAST` transitions to the callback as a [`PowerEvent`]:
///
/// | `wParam`                  | PowerEvent  |
/// |---------------------------|-------------|
/// | `PBT_APMSUSPEND`          | `WillSleep` |
/// | `PBT_APMRESUMESUSPEND`    | `DidWake`   |
/// | `PBT_APMRESUMEAUTOMATIC`  | `DidWake`   |
///
/// # Why a top-level window, not the message-only one
///
/// Like the volume observer (and unlike `start_system_theme_observer`),
/// power broadcasts only reach *top-level* windows, never the
/// `HWND_MESSAGE`-parented message-only kind. So this creates an
/// ordinary hidden top-level window (null parent, no `WS_VISIBLE`).
///
/// # Display sleep
///
/// Monitor on/off (`ScreensDidSleep` / `ScreensDidWake`) is **not**
/// covered here. On Windows that arrives as `PBT_POWERSETTINGCHANGE`
/// for the `GUID_CONSOLE_DISPLAY_STATE` power setting, which must first
/// be armed with `RegisterPowerSettingNotification(hwnd,
/// &GUID_CONSOLE_DISPLAY_STATE, DEVICE_NOTIFY_WINDOW_HANDLE)`; the
/// broadcast then carries a `POWERBROADCAST_SETTING` whose `Data[0]` is
/// 0=off / 1=on / 2=dimmed. Left as a follow-up — the macOS port
/// supplies screen events today, and system suspend/resume is the
/// higher-value signal.
///
/// # Thread contract
///
/// Same as [`start_volume_observer`]: **the callback fires on the
/// observer's worker thread** (hence `Send`), not the UI thread. It
/// must not touch gpui entities — the host marshals to the main thread
/// through a channel.
#[cfg(windows)]
pub fn start_power_observer(
    callback: Box<dyn Fn(ferail_core::power::PowerEvent) + 'static + Send>,
) {
    use ferail_core::power::PowerEvent;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW,
        RegisterClassExW, SetWindowLongPtrW, TranslateMessage, GWLP_USERDATA, HMENU, MSG,
        WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSEXW,
    };

    // Raw values (winuser.h) — windows-0.58 doesn't expose all as
    // typed constants, and the volume observer next door reads raw too.
    const WM_POWERBROADCAST: u32 = 0x0218;
    const PBT_APMSUSPEND: usize = 0x0004;
    const PBT_APMRESUMESUSPEND: usize = 0x0007;
    const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;

    type PowerCb = Box<dyn Fn(PowerEvent) + 'static + Send>;

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_POWERBROADCAST {
            let event = match wparam.0 {
                PBT_APMSUSPEND => Some(PowerEvent::WillSleep),
                PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC => Some(PowerEvent::DidWake),
                _ => None,
            };
            if let Some(event) = event {
                let user_data = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if user_data != 0 {
                    let cb_ptr = user_data as *const PowerCb;
                    (*cb_ptr)(event);
                }
            }
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    std::thread::Builder::new()
        .name("ferail-power-observer".into())
        .spawn(move || unsafe {
            let class_name: Vec<u16> = "FerailPowerObserver\0".encode_utf16().collect();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                hInstance: HINSTANCE::default(),
                lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
                ..Default::default()
            };
            let _ = RegisterClassExW(&wc);

            // Hidden top-level window (see doc: power broadcasts skip
            // message-only windows).
            let hwnd = match CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR::from_raw(class_name.as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND::default(),
                HMENU::default(),
                HINSTANCE::default(),
                None,
            ) {
                Ok(h) => h,
                Err(_) => return,
            };

            let cb_box: *mut PowerCb = Box::into_raw(Box::new(callback));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cb_box as isize);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .ok();
}

#[cfg(not(windows))]
pub fn start_power_observer(
    _callback: Box<dyn Fn(ferail_core::power::PowerEvent) + 'static + Send>,
) {
}

/// RAII guard that keeps the system awake while held, twin of
/// shell-mac's `SleepBlocker`. Dropping it re-allows idle sleep.
///
/// Implemented with `SetThreadExecutionState(ES_SYSTEM_REQUIRED |
/// ES_CONTINUOUS)`, cleared with a plain `ES_CONTINUOUS` on drop. That
/// flag is **per-thread and sticky** — it stays in effect (no activity
/// required) until reset on the *same* thread. The host holds the guard
/// inside one foreground task for the whole transfer, so set and clear
/// land on the same thread, which is the constraint this API needs.
///
/// If a future caller needs to assert from a thread-pool worker that
/// may not be the one that releases, switch to the Power Request API
/// (`PowerCreateRequest` / `PowerSetRequest(PowerRequestSystemRequired)`
/// / `PowerClearRequest` + `CloseHandle`), which is process-wide and
/// thread-independent.
#[cfg(windows)]
pub struct SleepBlocker {
    _private: (),
}

#[cfg(windows)]
impl Drop for SleepBlocker {
    fn drop(&mut self) {
        use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS};
        // Clearing the sticky bit: ES_CONTINUOUS alone, no
        // ES_SYSTEM_REQUIRED, drops back to default power behaviour.
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
}

/// Hold off idle system sleep until the returned guard drops. `reason`
/// is accepted for parity with the macOS assertion (which surfaces it
/// in `pmset -g assertions`); `SetThreadExecutionState` carries no
/// reason string, so it's unused here.
#[cfg(windows)]
pub fn prevent_idle_sleep(_reason: &str) -> Option<SleepBlocker> {
    use windows::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
    };
    // Returns the previous state (0 only on failure). Either way the
    // request is now in effect; hand back a guard that clears it.
    unsafe {
        SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
    }
    Some(SleepBlocker { _private: () })
}

#[cfg(not(windows))]
pub struct SleepBlocker;

#[cfg(not(windows))]
pub fn prevent_idle_sleep(_reason: &str) -> Option<SleepBlocker> {
    None
}

// Video — the macOS backend drives a windowless AVPlayer and hands the viewer
// BGRA frames (docs/features/VIEWER.md). On Windows these delegate to the Media
// Foundation `IMFMediaEngine` frame-server in `video_mf` (full A/V + sync); any
// failure returns handle 0 / None so the viewer falls back to the poster. The
// non-Windows arms are no-op stubs so the crate still compiles off Windows.

#[cfg(windows)]
pub fn video_overlay_show(path: &std::path::Path, on_ended: Box<dyn Fn() + 'static + Send>) -> u64 {
    video_mf::video_overlay_show(path, on_ended)
}
#[cfg(not(windows))]
pub fn video_overlay_show(
    _path: &std::path::Path,
    _on_ended: Box<dyn Fn() + 'static + Send>,
) -> u64 {
    0
}

#[cfg(windows)]
pub fn video_overlay_copy_frame(id: u64) -> Option<(u32, u32, Vec<u8>)> {
    video_mf::video_overlay_copy_frame(id)
}
#[cfg(not(windows))]
pub fn video_overlay_copy_frame(_id: u64) -> Option<(u32, u32, Vec<u8>)> {
    None
}

#[cfg(windows)]
pub fn video_overlay_remove(id: u64) {
    video_mf::video_overlay_remove(id)
}
#[cfg(not(windows))]
pub fn video_overlay_remove(_id: u64) {}

#[cfg(windows)]
pub fn video_overlay_set_paused(id: u64, paused: bool) {
    video_mf::video_overlay_set_paused(id, paused)
}
#[cfg(not(windows))]
pub fn video_overlay_set_paused(_id: u64, _paused: bool) {}

#[cfg(windows)]
pub fn video_overlay_set_muted(id: u64, muted: bool) {
    video_mf::video_overlay_set_muted(id, muted)
}
#[cfg(not(windows))]
pub fn video_overlay_set_muted(_id: u64, _muted: bool) {}

#[cfg(windows)]
pub fn video_overlay_restart(id: u64) {
    video_mf::video_overlay_restart(id)
}
#[cfg(not(windows))]
pub fn video_overlay_restart(_id: u64) {}

#[cfg(windows)]
pub fn video_overlay_time(id: u64) -> (f64, f64) {
    video_mf::video_overlay_time(id)
}
#[cfg(not(windows))]
pub fn video_overlay_time(_id: u64) -> (f64, f64) {
    (0.0, 0.0)
}

#[cfg(windows)]
pub fn video_overlay_natural_size(id: u64) -> (f64, f64) {
    video_mf::video_overlay_natural_size(id)
}
#[cfg(not(windows))]
pub fn video_overlay_natural_size(_id: u64) -> (f64, f64) {
    (0.0, 0.0)
}

#[cfg(windows)]
pub fn video_overlay_seek(id: u64, seconds: f64) {
    video_mf::video_overlay_seek(id, seconds)
}
#[cfg(not(windows))]
pub fn video_overlay_seek(_id: u64, _seconds: f64) {}

#[cfg(windows)]
pub fn video_overlay_step(id: u64, frames: i64) {
    video_mf::video_overlay_step(id, frames)
}
#[cfg(not(windows))]
pub fn video_overlay_step(_id: u64, _frames: i64) {}

/// Keep a window above every other app's, or stop doing so — the twin of
/// macOS's `NSWindow.level = .floating`. Powers the viewer's **Stay on Top**.
///
/// The pointer is whatever the platform's raw window handle carries: an
/// `NSView*` on macOS, an `HWND` here (both are pointers, so the shared
/// `platform_shell` signature holds).
///
/// `HWND_TOPMOST` / `HWND_NOTOPMOST` set and clear `WS_EX_TOPMOST` for us;
/// `SWP_NOMOVE | SWP_NOSIZE` means only the z-band changes, and
/// `SWP_NOACTIVATE` keeps a background viewer from stealing focus just because
/// the user ticked a checkbox.
#[cfg(windows)]
pub fn set_window_floating(hwnd_raw: *mut std::ffi::c_void, floating: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    if hwnd_raw.is_null() {
        return;
    }
    let hwnd = HWND(hwnd_raw);
    let band = if floating {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            band,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

#[cfg(not(windows))]
pub fn set_window_floating(_ns_view: *mut std::ffi::c_void, _floating: bool) {}

/// Set whole-window opacity, matching macOS `NSWindow.alphaValue`.
/// Keeps the layered style once enabled; other window effects may rely on it.
#[cfg(windows)]
pub fn set_window_opacity(hwnd_raw: *mut std::ffi::c_void, opacity: f32) {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, GWL_EXSTYLE, LWA_ALPHA,
        WS_EX_LAYERED,
    };
    if hwnd_raw.is_null() {
        return;
    }
    let hwnd = HWND(hwnd_raw);
    let alpha = (opacity.clamp(0.2, 1.0) * 255.0).round() as u8;
    unsafe {
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, current | WS_EX_LAYERED.0 as isize);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
    }
}

#[cfg(not(windows))]
pub fn set_window_opacity(_hwnd_raw: *mut std::ffi::c_void, _opacity: f32) {}

// Window docking primitives — still macOS-only (docs/features/DOCK.md). These
// stay no-op stubs deliberately: `dock.rs` computes frames in macOS *global
// screen space* (origin bottom-left, y-up), so a Windows port is not "fill in
// SetWindowPos" — it needs that coordinate space mapped onto Win32's top-left,
// y-down monitor rects, and a docked drawer cannot be verified headlessly.
// Implementing them blind is exactly how the `SetMuted` breakage happened
// (windows-port.md §2.2). `Shell::set_dock` early-returns on Windows because
// `window_ns_view` yields `None` there, so nothing half-works in the meantime;
// the plan is recorded in TODO.md § Cross-Platform.
pub fn current_mouse_location() -> (f64, f64) {
    (0.0, 0.0)
}
pub fn screen_visible_frame_for_window(
    _ns_view: *mut std::ffi::c_void,
) -> Option<(f64, f64, f64, f64)> {
    None
}
pub fn set_window_frame(_ns_view: *mut std::ffi::c_void, _x: f64, _y: f64, _w: f64, _h: f64) {}
pub fn set_window_all_spaces(_ns_view: *mut std::ffi::c_void, _all_spaces: bool) {}
pub fn window_frame(_ns_view: *mut std::ffi::c_void) -> Option<(f64, f64, f64, f64)> {
    None
}
pub fn window_is_fullscreen(_ns_view: *mut std::ffi::c_void) -> bool {
    false
}

// =============================================================
// Internal helpers
// =============================================================

/// Encode `s` as a null-terminated UTF-16 buffer for the Win32 W APIs.
#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(all(test, windows))]
mod win_tests {
    use super::*;

    /// make_alias_in drops a real `.lnk` (via the IShellLink COM path) into
    /// the destination folder, with a unique name.
    #[test]
    fn make_alias_in_creates_lnk_in_dest() {
        let base = std::env::temp_dir().join(format!("ferail-aliasin-{}", std::process::id()));
        let dest = base.join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let target = base.join("target.txt");
        std::fs::write(&target, b"hello").unwrap();

        let lnk = make_alias_in(&target, &dest).expect("alias created");
        assert!(lnk.exists(), "shortcut exists at {lnk:?}");
        assert_eq!(lnk.parent().unwrap(), dest, "shortcut landed in dest dir");
        assert_eq!(
            lnk.extension().and_then(|e| e.to_str()),
            Some("lnk"),
            "shortcut is a .lnk"
        );
        // A second alias of the same target gets a distinct name.
        let lnk2 = make_alias_in(&target, &dest).expect("second alias created");
        assert_ne!(lnk, lnk2, "second shortcut has a unique name");

        let _ = std::fs::remove_dir_all(&base);
    }
}

// ---------------------------------------------------------------------------
// Privilege escalation + locked-file diagnostics (resilient file operations).
// Real Win32 implementations in elevation.rs: ShellExecuteExW "runas" +
// Restart Manager. See docs/features/windows-port.md.
// ---------------------------------------------------------------------------

mod elevation;
pub use elevation::{
    elevation_available, force_close_processes, lock_diagnostics_available, processes_using,
    run_elevated_self, LockingProcess,
};
