//! Native-exception minidumps (WIN-001).
//!
//! Rust panics already leave `reports/ferail-crash-<pid>.txt` behind
//! (`ferail_gpui::obs`), but an access violation inside native code — a
//! GPU driver, a shell extension, a third-party preview handler in the
//! broker — never reaches the panic hook. This installs a top-level
//! unhandled-exception filter that writes a `MiniDumpWriteDump` file next
//! to the text report and appends one line naming the exception code and
//! address to that report, so a tester's dump plus the published PDBs
//! (`Ferail-<version>-win-x64-symbols.zip`) give a symbolized stack.
//!
//! The filter runs inside a process that just faulted, so it does as
//! little as possible: the destination path is pre-encoded at install
//! time, the dump goes through raw `CreateFileW`/`MiniDumpWriteDump`
//! handles, and only the best-effort sidecar line touches the Rust
//! standard library afterwards.
//!
//! Dump contents: thread stacks and module list (`MiniDumpNormal`), thread
//! info, handle data, and the **unloaded-module list** — the 0.6.5 report
//! faulted in an already-unloaded `pdfprevhndlr.dll`, which only that
//! stream can attribute.

#![cfg(windows)]

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
};
use windows::Win32::System::Diagnostics::Debug::{
    MiniDumpNormal, MiniDumpWithHandleData, MiniDumpWithThreadInfo, MiniDumpWithUnloadedModules,
    MiniDumpWriteDump, SetUnhandledExceptionFilter, EXCEPTION_POINTERS,
    MINIDUMP_EXCEPTION_INFORMATION, MINIDUMP_TYPE,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
};

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
const EXCEPTION_EXECUTE_HANDLER: i32 = 1;

struct Targets {
    dump_path_w: Vec<u16>,
    dump_path: PathBuf,
    sidecar: PathBuf,
    role: &'static str,
    quiet: bool,
}

static TARGETS: OnceLock<Targets> = OnceLock::new();

/// Install the process-wide unhandled-exception filter. Dumps land in
/// `reports_dir` as `ferail-<role>-<pid>.dmp`; the sidecar line goes to
/// `ferail-crash-<pid>.txt`, the same file the panic hook uses.
///
/// `quiet` decides what happens after the dump is written: `false` lets
/// the OS continue its default handling (Windows Error Reporting, a
/// debugger if attached) — right for the GUI; `true` terminates the
/// process straight away, which keeps a crashing preview helper from
/// popping WER UI on every affected file.
///
/// Returns the dump path, or `None` if the directory could not be created
/// or a filter was already installed by this function.
pub fn install_crash_dump_handler(
    reports_dir: &Path,
    role: &'static str,
    quiet: bool,
) -> Option<PathBuf> {
    std::fs::create_dir_all(reports_dir).ok()?;
    let pid = std::process::id();
    let dump_path = reports_dir.join(format!("ferail-{role}-{pid}.dmp"));
    let sidecar = reports_dir.join(format!("ferail-crash-{pid}.txt"));
    let mut dump_path_w: Vec<u16> = dump_path.as_os_str().encode_wide().collect();
    dump_path_w.push(0);
    TARGETS
        .set(Targets {
            dump_path_w,
            dump_path: dump_path.clone(),
            sidecar,
            role,
            quiet,
        })
        .ok()?;
    unsafe {
        SetUnhandledExceptionFilter(Some(filter));
    }
    Some(dump_path)
}

/// Re-arm Ferail's top-level filter after window/GPU initialization. Native
/// DLLs are allowed to replace `SetUnhandledExceptionFilter`; reinstalling at
/// a known post-load boundary keeps the GUI crash path covered. No-op before
/// [`install_crash_dump_handler`].
pub fn rearm_crash_dump_handler() -> bool {
    if TARGETS.get().is_none() {
        return false;
    }
    unsafe {
        SetUnhandledExceptionFilter(Some(filter));
    }
    true
}

unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
    let Some(t) = TARGETS.get() else {
        return EXCEPTION_CONTINUE_SEARCH;
    };

    let written = match CreateFileW(
        PCWSTR::from_raw(t.dump_path_w.as_ptr()),
        GENERIC_WRITE.0,
        FILE_SHARE_READ,
        None,
        CREATE_ALWAYS,
        FILE_ATTRIBUTE_NORMAL,
        None,
    ) {
        Ok(file) => {
            let exception = MINIDUMP_EXCEPTION_INFORMATION {
                ThreadId: GetCurrentThreadId(),
                ExceptionPointers: info as *mut EXCEPTION_POINTERS,
                ClientPointers: false.into(),
            };
            let kind = MINIDUMP_TYPE(
                MiniDumpNormal.0
                    | MiniDumpWithThreadInfo.0
                    | MiniDumpWithUnloadedModules.0
                    | MiniDumpWithHandleData.0,
            );
            let ok = MiniDumpWriteDump(
                GetCurrentProcess(),
                GetCurrentProcessId(),
                file,
                kind,
                Some(&exception),
                None,
                None,
            )
            .is_ok();
            let _ = CloseHandle(file);
            ok
        }
        Err(_) => false,
    };

    // Best effort from here on: the dump is already on disk.
    let (code, address) = if info.is_null() || (*info).ExceptionRecord.is_null() {
        (0u32, 0usize)
    } else {
        let record = &*(*info).ExceptionRecord;
        (
            record.ExceptionCode.0 as u32,
            record.ExceptionAddress as usize,
        )
    };
    append_sidecar(t, written, code, address);

    if t.quiet {
        EXCEPTION_EXECUTE_HANDLER
    } else {
        EXCEPTION_CONTINUE_SEARCH
    }
}

fn append_sidecar(t: &Targets, written: bool, code: u32, address: usize) {
    use std::io::Write as _;
    let line = format!(
        "native exception 0x{code:08X} at 0x{address:016X} in {} (pid {}); minidump {}: {}",
        t.role,
        std::process::id(),
        if written { "written" } else { "FAILED" },
        t.dump_path.display(),
    );
    // A native fault killed the process with nothing on the console; when
    // launched from a terminal, this one line says what happened and where
    // the dump is. Quiet mode (the preview broker, one process per file)
    // stays silent. Best-effort, like the sidecar itself.
    if !t.quiet {
        let _ = writeln!(std::io::stderr(), "{line}");
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&t.sidecar)
    else {
        return;
    };
    let _ = writeln!(file, "{line}");
}
