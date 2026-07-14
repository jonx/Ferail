//! Privilege escalation + locked-file diagnostics (resilient file operations).
//!
//! Three primitives behind `platform_shell`:
//!
//! - [`run_elevated_self`] — re-launch this binary elevated via
//!   `ShellExecuteExW` verb `"runas"` (the UAC prompt), wait for the child,
//!   and return its exit code. The caller ([`feraille-gpui`'s `elevation`
//!   module]) owns the descriptor/result-file handshake; this function never
//!   sees the op type.
//! - [`processes_using`] — name the processes holding a file open, via the
//!   Restart Manager (`RmStartSession` → `RmRegisterResources` → `RmGetList`).
//! - [`force_close_processes`] — ask those processes to close:
//!   Restart-Manager graceful shutdown first (`RmShutdown`, which delivers
//!   WM_CLOSE to GUI apps), then a `TerminateProcess` fallback for survivors.
//!
//! All three block (auth dialog / RM enumeration can take seconds) — call
//! them from a background executor, never the UI thread (Prime Directive).

/// A process holding a file open. Identical shape in every shell crate.
#[derive(Clone, Debug)]
pub struct LockingProcess {
    pub pid: u32,
    pub name: String,
}

/// Whether "Retry as administrator…" can work here.
pub fn elevation_available() -> bool {
    cfg!(windows)
}

/// Whether [`processes_using`] can actually answer here.
pub fn lock_diagnostics_available() -> bool {
    cfg!(windows)
}

#[cfg(windows)]
pub use imp::{force_close_processes, processes_using, run_elevated_self};

#[cfg(not(windows))]
pub fn run_elevated_self(_args: &[String]) -> Result<i32, String> {
    Err("elevation is not available on this platform".into())
}

#[cfg(not(windows))]
pub fn processes_using(_path: &std::path::Path) -> Vec<LockingProcess> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn force_close_processes(_pids: &[u32]) -> Result<(), String> {
    Err("closing the locking process isn't supported on this platform".into())
}

#[cfg(windows)]
mod imp {
    use super::LockingProcess;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::core::PWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_CANCELLED, ERROR_MORE_DATA, ERROR_SUCCESS, FILETIME, HANDLE,
        STILL_ACTIVE,
    };
    use windows::Win32::System::RestartManager::{
        RmEndSession, RmForceShutdown, RmGetList, RmRegisterResources, RmShutdown,
        RmStartSession, CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RM_UNIQUE_PROCESS,
    };
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, TerminateProcess,
        WaitForSingleObject, INFINITE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SEE_MASK_NO_CONSOLE,
        SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    /// Append one argument to a command line under `CommandLineToArgvW`
    /// quoting rules, so the elevated child's `std::env::args` round-trips
    /// exactly (descriptor paths can contain spaces).
    fn append_quoted(arg: &str, cmdline: &mut String) {
        let needs_quotes =
            arg.is_empty() || arg.chars().any(|c| matches!(c, ' ' | '\t' | '\n' | '"'));
        if !needs_quotes {
            cmdline.push_str(arg);
            return;
        }
        cmdline.push('"');
        let chars: Vec<char> = arg.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let mut backslashes = 0;
            while i < chars.len() && chars[i] == '\\' {
                backslashes += 1;
                i += 1;
            }
            if i == chars.len() {
                // Backslashes before the closing quote must be doubled.
                for _ in 0..backslashes * 2 {
                    cmdline.push('\\');
                }
                break;
            } else if chars[i] == '"' {
                for _ in 0..backslashes * 2 + 1 {
                    cmdline.push('\\');
                }
                cmdline.push('"');
            } else {
                for _ in 0..backslashes {
                    cmdline.push('\\');
                }
                cmdline.push(chars[i]);
            }
            i += 1;
        }
        cmdline.push('"');
    }

    /// UAC re-exec: launch `current_exe()` with `args` via the `"runas"`
    /// verb, wait for it to exit, return its exit code. `Err("cancelled")`
    /// when the user dismissed the UAC prompt (matches the macOS contract).
    pub fn run_elevated_self(args: &[String]) -> Result<i32, String> {
        let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
        let mut params = String::new();
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                params.push(' ');
            }
            append_quoted(a, &mut params);
        }
        // These buffers must outlive the ShellExecuteExW call.
        let exe_w = wide(exe.as_os_str());
        let params_w = wide(std::ffi::OsStr::new(&params));
        let verb_w = wide(std::ffi::OsStr::new("runas"));

        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            // NOCLOSEPROCESS: hand back a process handle to wait on.
            // NO_CONSOLE + SW_HIDE: no flash of a console window.
            // NOASYNC: fully resolve before returning (we block anyway).
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE | SEE_MASK_NOASYNC,
            lpVerb: windows::core::PCWSTR::from_raw(verb_w.as_ptr()),
            lpFile: windows::core::PCWSTR::from_raw(exe_w.as_ptr()),
            lpParameters: windows::core::PCWSTR::from_raw(params_w.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
            return Err(if e.code() == ERROR_CANCELLED.to_hresult() {
                "cancelled".into()
            } else {
                format!("ShellExecuteEx(runas): {e}")
            });
        }
        let child = info.hProcess;
        if child.is_invalid() {
            return Err("ShellExecuteEx(runas): no process handle".into());
        }
        unsafe { WaitForSingleObject(child, INFINITE) };
        let mut code = 0u32;
        let got = unsafe { GetExitCodeProcess(child, &mut code) };
        unsafe {
            let _ = CloseHandle(child);
        }
        got.map_err(|e| format!("GetExitCodeProcess: {e}"))?;
        Ok(code as i32)
    }

    /// A Restart Manager session that always ends, whatever path returns.
    struct RmSession(u32);
    impl RmSession {
        fn start() -> Option<Self> {
            let mut handle = 0u32;
            let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
            let err = unsafe { RmStartSession(&mut handle, 0, PWSTR(key.as_mut_ptr())) };
            (err == ERROR_SUCCESS).then_some(Self(handle))
        }
    }
    impl Drop for RmSession {
        fn drop(&mut self) {
            unsafe {
                let _ = RmEndSession(self.0);
            }
        }
    }

    /// Name the processes holding `path` open. Empty on any failure — this
    /// feeds a diagnostic list, not control flow, so "don't know" and "none"
    /// render the same. Blocks (RM enumerates every process): background only.
    pub fn processes_using(path: &Path) -> Vec<LockingProcess> {
        let Some(session) = RmSession::start() else {
            return Vec::new();
        };
        let path_w = wide(path.as_os_str());
        let names = [windows::core::PCWSTR::from_raw(path_w.as_ptr())];
        if unsafe { RmRegisterResources(session.0, Some(&names), None, None) } != ERROR_SUCCESS {
            return Vec::new();
        }
        let mut needed = 0u32;
        let mut count = 0u32;
        let mut reasons = 0u32;
        let probe = unsafe { RmGetList(session.0, &mut needed, &mut count, None, &mut reasons) };
        if probe == ERROR_SUCCESS || needed == 0 {
            return Vec::new(); // nobody holds it
        }
        if probe != ERROR_MORE_DATA {
            return Vec::new();
        }
        let mut infos = vec![RM_PROCESS_INFO::default(); needed as usize];
        let mut count = needed;
        let err = unsafe {
            RmGetList(
                session.0,
                &mut needed,
                &mut count,
                Some(infos.as_mut_ptr()),
                &mut reasons,
            )
        };
        if err != ERROR_SUCCESS {
            return Vec::new();
        }
        infos.truncate(count as usize);
        infos
            .iter()
            .map(|info| {
                let name_len = info
                    .strAppName
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(info.strAppName.len());
                let mut name = String::from_utf16_lossy(&info.strAppName[..name_len]);
                if name.is_empty() {
                    name = format!("pid {}", info.Process.dwProcessId);
                }
                LockingProcess {
                    pid: info.Process.dwProcessId,
                    name,
                }
            })
            .collect()
    }

    fn open_query(pid: u32) -> Option<HANDLE> {
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()
    }

    fn start_time(pid: u32) -> Option<FILETIME> {
        let h = open_query(pid)?;
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let got =
            unsafe { GetProcessTimes(h, &mut creation, &mut exit, &mut kernel, &mut user) };
        unsafe {
            let _ = CloseHandle(h);
        }
        got.ok().map(|()| creation)
    }

    fn alive(pid: u32) -> bool {
        let Some(h) = open_query(pid) else {
            return false;
        };
        let mut code = 0u32;
        let got = unsafe { GetExitCodeProcess(h, &mut code) };
        unsafe {
            let _ = CloseHandle(h);
        }
        got.is_ok() && code == STILL_ACTIVE.0 as u32
    }

    /// Close the given processes so a locked file can be retried. Graceful
    /// Restart-Manager shutdown first (WM_CLOSE, service stop), then
    /// `TerminateProcess` for whatever survived. Blocks: background only.
    pub fn force_close_processes(pids: &[u32]) -> Result<(), String> {
        if pids.is_empty() {
            return Ok(());
        }
        // Polite pass: RM identifies apps by (pid, start time) so a recycled
        // pid can't kill an innocent process.
        let apps: Vec<RM_UNIQUE_PROCESS> = pids
            .iter()
            .filter_map(|&pid| {
                start_time(pid).map(|t| RM_UNIQUE_PROCESS {
                    dwProcessId: pid,
                    ProcessStartTime: t,
                })
            })
            .collect();
        if !apps.is_empty() {
            if let Some(session) = RmSession::start() {
                if unsafe { RmRegisterResources(session.0, None, Some(&apps), None) }
                    == ERROR_SUCCESS
                {
                    let _ = unsafe { RmShutdown(session.0, RmForceShutdown.0 as u32, None) };
                }
            }
        }
        // Hard pass for survivors.
        let mut failed: Vec<u32> = Vec::new();
        for &pid in pids {
            if !alive(pid) {
                continue;
            }
            let Ok(h) = (unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) }) else {
                failed.push(pid);
                continue;
            };
            let killed = unsafe { TerminateProcess(h, 1) };
            if killed.is_ok() {
                // Give the kernel a beat so an immediate retry doesn't race
                // the handle teardown.
                unsafe { WaitForSingleObject(h, 3_000) };
            }
            unsafe {
                let _ = CloseHandle(h);
            }
            if killed.is_err() {
                failed.push(pid);
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "could not close: {}",
                failed
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::append_quoted;

        fn quoted(s: &str) -> String {
            let mut out = String::new();
            append_quoted(s, &mut out);
            out
        }

        #[test]
        fn plain_args_pass_through() {
            assert_eq!(quoted(r"C:\temp\file.desc"), r"C:\temp\file.desc");
        }

        #[test]
        fn spaces_get_quoted() {
            assert_eq!(quoted(r"C:\my dir\op.desc"), r#""C:\my dir\op.desc""#);
        }

        #[test]
        fn trailing_backslash_inside_quotes_is_doubled() {
            assert_eq!(quoted(r"C:\my dir\"), r#""C:\my dir\\""#);
        }

        #[test]
        fn embedded_quote_is_escaped() {
            assert_eq!(quoted(r#"a"b"#), r#""a\"b""#);
        }

        #[test]
        fn empty_arg_stays_an_arg() {
            assert_eq!(quoted(""), r#""""#);
        }
    }
}
