//! Isolated Windows Shell context-menu broker (WIN-007).
//!
//! The GUI only launches this role after an explicit user gesture. All
//! `IContextMenu` implementations — including third-party Explorer extensions
//! — live in this disposable process, never in Ferail's GPUI process.

use std::{
    cell::RefCell,
    ffi::{c_void, OsString},
    io::{BufRead as _, BufReader, Write as _},
    os::windows::ffi::OsStrExt,
    os::windows::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{mpsc, OnceLock},
    time::{Duration, Instant},
};

use windows::{
    core::{Interface, PCSTR, PCWSTR, PSTR},
    Win32::{
        Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::{
            Com::{CoTaskMemFree, IBindCtx},
            Ole::{OleInitialize, OleUninitialize},
        },
        UI::{
            Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT},
            Shell::{
                IContextMenu, IContextMenu2, IContextMenu3, IShellFolder, SHBindToParent,
                SHParseDisplayName, CMF_EXTENDEDVERBS, CMF_NORMAL, CMIC_MASK_CONTROL_DOWN,
                CMIC_MASK_PTINVOKE, CMIC_MASK_SHIFT_DOWN, CMINVOKECOMMANDINFO,
                CMINVOKECOMMANDINFOEX, GCS_VERBA, GCS_VERBW, SEE_MASK_NOASYNC, SEE_MASK_NO_CONSOLE,
                SEE_MASK_UNICODE,
            },
            WindowsAndMessaging::{
                CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
                DispatchMessageW, EnumWindows, GetCursorPos, GetWindow, IsWindow,
                MsgWaitForMultipleObjectsEx, PeekMessageW, PostMessageW, RegisterClassExW,
                SetForegroundWindow, ShowWindow, TrackPopupMenuEx, TranslateMessage, GW_OWNER,
                HMENU, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, SW_SHOWNORMAL,
                TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM,
                WM_MENUCHAR, WM_NULL, WM_QUIT, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
            },
        },
    },
};

use windows::Win32::UI::Shell::Common::ITEMIDLIST;

const FIRST_COMMAND_ID: u32 = 1;
const LAST_COMMAND_ID: u32 = 0x7fff;
const BROKER_ARG: &str = "--windows-context-menu-broker";
const READY_MARKER: &str = "FERAIL_CONTEXT_MENU_READY";
const PREPARE_TIMEOUT: Duration = Duration::from_secs(8);
const PROPERTY_DIALOG_APPEAR_TIMEOUT: Duration = Duration::from_secs(3);
const SYNCHRONOUS_PROPERTY_INVOKE_FLOOR: Duration = Duration::from_millis(250);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Default)]
struct ActiveMenu {
    menu2: Option<IContextMenu2>,
    menu3: Option<IContextMenu3>,
}

struct ActiveMenuGuard;

impl ActiveMenuGuard {
    fn install(menu2: Option<IContextMenu2>, menu3: Option<IContextMenu3>) -> Self {
        ACTIVE_MENU.with(|slot| *slot.borrow_mut() = ActiveMenu { menu2, menu3 });
        Self
    }
}

impl Drop for ActiveMenuGuard {
    fn drop(&mut self) {
        ACTIVE_MENU.with(|slot| *slot.borrow_mut() = ActiveMenu::default());
    }
}

thread_local! {
    static ACTIVE_MENU: RefCell<ActiveMenu> = RefCell::new(ActiveMenu::default());
}

struct AbsolutePidl(*mut ITEMIDLIST);

impl Drop for AbsolutePidl {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast::<c_void>())) };
    }
}

struct OwnedMenu(HMENU);

impl Drop for OwnedMenu {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
}

struct OwnerWindow(HWND);

impl OwnerWindow {
    fn new(point: POINT) -> windows::core::Result<Self> {
        static CLASS_REGISTERED: OnceLock<()> = OnceLock::new();
        static CLASS_NAME: OnceLock<Vec<u16>> = OnceLock::new();
        let class_name =
            CLASS_NAME.get_or_init(|| "FerailShellContextMenuBroker\0".encode_utf16().collect());

        CLASS_REGISTERED.get_or_init(|| unsafe {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(context_menu_wnd_proc),
                hInstance: HINSTANCE::default(),
                lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
                ..Default::default()
            };
            let _ = RegisterClassExW(&class);
        });

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                PCWSTR::from_raw(class_name.as_ptr()),
                PCWSTR::null(),
                WS_POPUP | WS_VISIBLE,
                point.x,
                point.y,
                1,
                1,
                HWND::default(),
                HMENU::default(),
                HINSTANCE::default(),
                None,
            )?
        };
        Ok(Self(hwnd))
    }
}

impl Drop for OwnerWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

unsafe extern "system" fn context_menu_wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if matches!(
        message,
        WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR
    ) {
        let active = ACTIVE_MENU.with(|slot| slot.borrow().clone());
        if let Some(menu3) = active.menu3 {
            let mut result = LRESULT::default();
            if unsafe { menu3.HandleMenuMsg2(message, wparam, lparam, Some(&mut result)) }.is_ok() {
                return result;
            }
        }
        if let Some(menu2) = active.menu2 {
            if unsafe { menu2.HandleMenuMsg(message, wparam, lparam) }.is_ok() {
                return LRESULT::default();
            }
        }
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn parse_path(path: &Path) -> Result<AbsolutePidl, String> {
    let shell_path = super::strip_verbatim(path);
    let wide: Vec<u16> = shell_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut pidl = std::ptr::null_mut();
    unsafe {
        SHParseDisplayName(
            PCWSTR::from_raw(wide.as_ptr()),
            None::<&IBindCtx>,
            &mut pidl,
            0,
            None,
        )
    }
    .map_err(|error| format!("Shell could not parse {}: {error}", shell_path.display()))?;
    if pidl.is_null() {
        return Err(format!(
            "Shell returned no item identity for {}",
            shell_path.display()
        ));
    }
    Ok(AbsolutePidl(pidl))
}

fn same_parent(paths: &[PathBuf]) -> bool {
    let Some(parent) = paths.first().and_then(|path| path.parent()) else {
        return false;
    };
    paths
        .iter()
        .all(|path| path.parent().is_some_and(|candidate| candidate == parent))
}

/// Ask the menu provider for the locale-independent verb belonging to an
/// offset. This is optional for third-party providers, so failure simply
/// means that normal offset invocation remains in charge.
fn canonical_verb(context_menu: &IContextMenu, offset: u32) -> Option<String> {
    let mut wide = [0u16; 128];
    if unsafe {
        context_menu.GetCommandString(
            offset as usize,
            GCS_VERBW,
            None,
            PSTR(wide.as_mut_ptr().cast()),
            wide.len() as u32,
        )
    }
    .is_ok()
    {
        let len = wide.iter().position(|ch| *ch == 0).unwrap_or(wide.len());
        if len != 0 {
            return Some(String::from_utf16_lossy(&wide[..len]));
        }
    }

    // A few legacy handlers only implement the ANSI query. Canonical Shell
    // verbs are ASCII, so a lossy conversion is sufficient for comparison.
    let mut ansi = [0u8; 128];
    if unsafe {
        context_menu.GetCommandString(
            offset as usize,
            GCS_VERBA,
            None,
            PSTR(ansi.as_mut_ptr()),
            ansi.len() as u32,
        )
    }
    .is_ok()
    {
        let len = ansi.iter().position(|ch| *ch == 0).unwrap_or(ansi.len());
        if len != 0 {
            return Some(String::from_utf8_lossy(&ansi[..len]).into_owned());
        }
    }
    None
}

/// Some Shell verbs, notably Properties, create a modeless owned window and
/// return from `InvokeCommand` immediately even when `CMIC_MASK_NOASYNC` was
/// requested. The disposable broker must therefore keep its STA/message pump
/// alive until that window closes. No timeout applies after the dialog appears:
/// it is user-modal and may legitimately stay open for minutes.
fn owned_popup_exists(owner: HWND) -> bool {
    struct Search {
        owner: HWND,
        found: bool,
    }

    unsafe extern "system" fn visit(window: HWND, state: LPARAM) -> BOOL {
        let state = unsafe { &mut *(state.0 as *mut Search) };
        if window != state.owner
            && unsafe { IsWindow(window) }.as_bool()
            && unsafe { GetWindow(window, GW_OWNER) }.ok() == Some(state.owner)
        {
            state.found = true;
            return false.into();
        }
        true.into()
    }

    let mut search = Search {
        owner,
        found: false,
    };
    unsafe {
        let _ = EnumWindows(
            Some(visit),
            LPARAM((&mut search as *mut Search).cast::<()>() as isize),
        );
    }
    search.found
}

fn pump_owned_property_dialog(owner: HWND) -> Result<(), String> {
    let appear_deadline = Instant::now() + PROPERTY_DIALOG_APPEAR_TIMEOUT;
    let mut appeared = false;

    loop {
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, HWND::default(), 0, 0, PM_REMOVE) }.as_bool() {
            if message.message == WM_QUIT {
                return Err("the Properties message loop quit unexpectedly".into());
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        if owned_popup_exists(owner) {
            appeared = true;
        } else if appeared {
            return Ok(());
        } else if Instant::now() >= appear_deadline {
            // The selected handler may have completed synchronously or handed
            // the sheet to another process. InvokeCommand already succeeded,
            // so absence of our own popup is not an error.
            return Ok(());
        }

        unsafe {
            MsgWaitForMultipleObjectsEx(None, 100, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }
    }
}

/// Launch the disposable broker and wait for its native menu to close.
/// Call from a background executor: the menu is intentionally user-modal.
pub fn show_windows_context_menu(paths: &[PathBuf], extended: bool) -> Result<(), String> {
    if paths.is_empty() {
        return Err("the Windows menu needs at least one filesystem item".into());
    }
    if !same_parent(paths) {
        return Err("Windows can only build one native menu for items in the same folder".into());
    }

    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command = Command::new(exe);
    command.arg(BROKER_ARG);
    if extended {
        command.arg("--extended");
    }
    command
        .args(paths)
        .stdout(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "context-menu broker did not expose its readiness pipe".to_string())?;
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let ready = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .any(|line| line.trim() == READY_MARKER);
        let _ = ready_tx.send(ready);
    });

    match ready_rx.recv_timeout(PREPARE_TIMEOUT) {
        Ok(true) => {}
        Ok(false) => {
            let status = child.wait().map_err(|error| error.to_string())?;
            return Err(format!(
                "Windows context-menu broker exited before its menu was ready ({status})"
            ));
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("a Windows Shell extension took too long to build the menu".into());
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Windows context-menu broker closed its readiness pipe".into());
        }
    }

    // Once the popup is ready it is deliberately user-modal: do not apply a
    // timeout while the user is reading a submenu or deciding what to invoke.
    let status = child.wait().map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Windows context-menu broker exited with {status}"))
    }
}

/// Entry point for the `--windows-context-menu-broker` process role.
pub fn context_menu_broker_main(args: &[OsString]) -> i32 {
    let (extended, path_args) = match args.first().and_then(|arg| arg.to_str()) {
        Some("--extended") => (true, &args[1..]),
        _ => (false, args),
    };
    let paths = path_args.iter().map(PathBuf::from).collect::<Vec<_>>();
    if paths.is_empty() || !same_parent(&paths) {
        eprintln!("ferail: Windows context menu requires same-folder paths");
        return 2;
    }

    if let Err(error) = unsafe { OleInitialize(None) } {
        eprintln!("ferail: unable to initialize OLE for context menu: {error}");
        return 1;
    }
    let result = show_menu(&paths, extended);
    unsafe { OleUninitialize() };
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("ferail: Windows context menu failed: {error}");
            1
        }
    }
}

fn show_menu(paths: &[PathBuf], extended: bool) -> Result<(), String> {
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point).map_err(|error| error.to_string())? };
    // InvokeCommand is allowed to parent dialogs (notably Properties) to the
    // supplied HWND. Keep the broker's 1px tool window at the invocation
    // point: an off-screen owner can make those dialogs open off-screen too.
    let owner = OwnerWindow::new(point).map_err(|error| error.to_string())?;
    let pidls = paths
        .iter()
        .map(|path| parse_path(path))
        .collect::<Result<Vec<_>, _>>()?;

    let mut first_child = std::ptr::null_mut();
    let folder: IShellFolder = unsafe { SHBindToParent(pidls[0].0, Some(&mut first_child)) }
        .map_err(|error| format!("could not bind the selected items' parent: {error}"))?;
    let children = pidls
        .iter()
        .map(|pidl| unsafe { windows::Win32::UI::Shell::ILFindLastID(pidl.0) }.cast_const())
        .collect::<Vec<_>>();
    let context_menu: IContextMenu = unsafe { folder.GetUIObjectOf(owner.0, &children, None) }
        .map_err(|error| format!("could not obtain the Shell context menu: {error}"))?;

    let menu2 = context_menu.cast::<IContextMenu2>().ok();
    let menu3 = context_menu.cast::<IContextMenu3>().ok();
    let _active_menu = ActiveMenuGuard::install(menu2, menu3);

    let popup = OwnedMenu(unsafe { CreatePopupMenu() }.map_err(|error| error.to_string())?);
    let flags = CMF_NORMAL | if extended { CMF_EXTENDEDVERBS } else { 0 };
    let query = unsafe {
        context_menu.QueryContextMenu(popup.0, 0, FIRST_COMMAND_ID, LAST_COMMAND_ID, flags)
    };
    query.map_err(|error| format!("Shell extension menu enumeration failed: {error}"))?;

    // Parent only bounds provider enumeration. Everything after this point is
    // the normal user-modal popup lifetime and may last as long as the user
    // wants. Flush because stdout is a private readiness pipe, not a console.
    println!("{READY_MARKER}");
    std::io::stdout()
        .flush()
        .map_err(|error| format!("could not signal menu readiness: {error}"))?;

    unsafe {
        let _ = ShowWindow(owner.0, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(owner.0);
    }
    let selected = unsafe {
        TrackPopupMenuEx(
            popup.0,
            (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
            point.x,
            point.y,
            owner.0,
            None,
        )
    }
    .0 as u32;
    unsafe {
        let _ = PostMessageW(owner.0, WM_NULL, WPARAM::default(), LPARAM::default());
    }

    if selected >= FIRST_COMMAND_ID {
        let offset = selected - FIRST_COMMAND_ID;
        let is_properties = canonical_verb(&context_menu, offset)
            .is_some_and(|verb| verb.eq_ignore_ascii_case("properties"));
        // This process is deliberately disposable. Ask handlers to finish
        // synchronously so a modeless in-proc command (Properties is the
        // canonical example) cannot be torn down when the broker exits.
        // SEE_MASK_* and CMIC_MASK_* are the same documented bit values; the
        // windows crate exposes these three aliases under their SEE names.
        let mut mask =
            CMIC_MASK_PTINVOKE | SEE_MASK_UNICODE | SEE_MASK_NO_CONSOLE | SEE_MASK_NOASYNC;
        if unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 {
            mask |= CMIC_MASK_CONTROL_DOWN;
        }
        if unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0 {
            mask |= CMIC_MASK_SHIFT_DOWN;
        }
        let info = CMINVOKECOMMANDINFOEX {
            cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
            fMask: mask,
            hwnd: owner.0,
            lpVerb: PCSTR(offset as usize as *const u8),
            lpVerbW: PCWSTR(offset as usize as *const u16),
            nShow: SW_SHOWNORMAL.0,
            ptInvoke: point,
            ..Default::default()
        };
        let invoke_started = Instant::now();
        unsafe {
            context_menu
                .InvokeCommand(&info as *const CMINVOKECOMMANDINFOEX as *const CMINVOKECOMMANDINFO)
        }
        .map_err(|error| format!("Shell command failed: {error}"))?;
        if is_properties
            && (owned_popup_exists(owner.0)
                || invoke_started.elapsed() < SYNCHRONOUS_PROPERTY_INVOKE_FLOOR)
        {
            pump_owned_property_dialog(owner.0)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::same_parent;
    use std::path::PathBuf;

    #[test]
    fn native_menu_selection_must_share_one_parent() {
        assert!(same_parent(&[
            PathBuf::from(r"C:\one\a.txt"),
            PathBuf::from(r"C:\one\b.txt"),
        ]));
        assert!(!same_parent(&[
            PathBuf::from(r"C:\one\a.txt"),
            PathBuf::from(r"C:\two\b.txt"),
        ]));
    }
}
