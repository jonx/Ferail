use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use ferail_core::platform_shortcuts::{
    ShortcutFailureKind, ShortcutInfo, ShortcutResolveRequest, ShortcutResolver, ShortcutTarget,
    ShortcutTargetKind,
};
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, STGM,
};
use windows::Win32::UI::Shell::{
    CommandLineToArgvW, IShellLinkW, ShellLink, SLGP_RAWPATH, SLR_NOSEARCH, SLR_NOTRACK,
    SLR_NOUPDATE, SLR_NO_UI,
};

const WIDE_BUFFER_LEN: usize = 32_768;

pub struct WindowsShortcutResolver;

impl ShortcutResolver for WindowsShortcutResolver {
    fn resolve(
        &self,
        request: ShortcutResolveRequest,
        cancel: &AtomicBool,
    ) -> Result<ShortcutInfo, ShortcutFailureKind> {
        if cancel.load(Ordering::Relaxed) {
            return Err(ShortcutFailureKind::Cancelled);
        }
        // The GPUI/background executor may already have initialized a pool
        // thread as MTA. ShellLink resolution is deliberately isolated on a
        // fresh STA so COM apartment state is deterministic and fully drops
        // before this provider slot is released.
        std::thread::scope(|scope| {
            scope
                .spawn(|| resolve_on_sta(request, cancel))
                .join()
                .unwrap_or(Err(ShortcutFailureKind::Failed))
        })
    }
}

fn resolve_on_sta(
    request: ShortcutResolveRequest,
    cancel: &AtomicBool,
) -> Result<ShortcutInfo, ShortcutFailureKind> {
    if request
        .source
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("lnk"))
    {
        return Err(ShortcutFailureKind::Unsupported);
    }
    let source: Vec<u16> = request
        .source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|_| ShortcutFailureKind::Failed)?;
        let result = (|| {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|_| ShortcutFailureKind::Failed)?;
            let persist: IPersistFile = link.cast().map_err(|_| ShortcutFailureKind::Failed)?;
            persist
                .Load(PCWSTR::from_raw(source.as_ptr()), STGM(0))
                .map_err(map_load_error)?;
            if cancel.load(Ordering::Relaxed) {
                return Err(ShortcutFailureKind::Cancelled);
            }

            let resolve_flags = (SLR_NO_UI | SLR_NOSEARCH | SLR_NOTRACK | SLR_NOUPDATE).0 as u32;
            // Resolve is best-effort: a broken link may still expose its last
            // target through GetPath, which lets us report TargetMissing rather
            // than flattening every failure into an opaque COM error.
            let _ = link.Resolve(None, resolve_flags);

            let mut target_buffer = vec![0u16; WIDE_BUFFER_LEN];
            link.GetPath(
                &mut target_buffer,
                std::ptr::null_mut(),
                SLGP_RAWPATH.0 as u32,
            )
            .map_err(|_| ShortcutFailureKind::Broken)?;
            let target = path_from_buffer(&target_buffer).ok_or(ShortcutFailureKind::Broken)?;
            if cancel.load(Ordering::Relaxed) {
                return Err(ShortcutFailureKind::Cancelled);
            }
            let metadata = std::fs::metadata(&target).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => ShortcutFailureKind::TargetMissing,
                std::io::ErrorKind::PermissionDenied => ShortcutFailureKind::PermissionDenied,
                _ => ShortcutFailureKind::Failed,
            })?;
            let kind = if metadata.is_dir() {
                ShortcutTargetKind::Directory
            } else if target
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "exe" | "com" | "bat" | "cmd" | "msi"
                    )
                })
            {
                ShortcutTargetKind::Application
            } else if metadata.is_file() {
                ShortcutTargetKind::File
            } else {
                ShortcutTargetKind::Other
            };

            let mut arguments_buffer = vec![0u16; WIDE_BUFFER_LEN];
            let arguments = if link.GetArguments(&mut arguments_buffer).is_ok() {
                parse_arguments(&arguments_buffer)
            } else {
                Vec::new()
            };
            let mut working_buffer = vec![0u16; WIDE_BUFFER_LEN];
            let working_directory = link
                .GetWorkingDirectory(&mut working_buffer)
                .ok()
                .and_then(|_| path_from_buffer(&working_buffer));
            let mut icon_buffer = vec![0u16; WIDE_BUFFER_LEN];
            let mut icon_index = 0;
            let icon_location = link
                .GetIconLocation(&mut icon_buffer, &mut icon_index)
                .ok()
                .and_then(|_| path_from_buffer(&icon_buffer))
                .map(|path| (path, icon_index));

            Ok(ShortcutInfo {
                target: Ok(ShortcutTarget::FileSystem { path: target, kind }),
                arguments,
                working_directory,
                icon_location,
            })
        })();
        CoUninitialize();
        result
    }
}

fn map_load_error(error: windows::core::Error) -> ShortcutFailureKind {
    let code = error.code();
    if code == windows::Win32::Foundation::E_ACCESSDENIED {
        ShortcutFailureKind::PermissionDenied
    } else {
        ShortcutFailureKind::Broken
    }
}

fn path_from_buffer(buffer: &[u16]) -> Option<PathBuf> {
    let end = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    (end > 0).then(|| PathBuf::from(OsString::from_wide(&buffer[..end])))
}

fn parse_arguments(buffer: &[u16]) -> Vec<OsString> {
    let end = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    if end == 0 {
        return Vec::new();
    }
    let mut command_line = buffer[..end].to_vec();
    command_line.push(0);
    let mut count = 0;
    let argv = unsafe { CommandLineToArgvW(PCWSTR::from_raw(command_line.as_ptr()), &mut count) };
    if argv.is_null() || count <= 0 {
        return vec![OsString::from_wide(&buffer[..end])];
    }
    let arguments = unsafe {
        std::slice::from_raw_parts(argv, count as usize)
            .iter()
            .map(|argument| {
                let mut len = 0;
                while *argument.0.add(len) != 0 {
                    len += 1;
                }
                OsString::from_wide(std::slice::from_raw_parts(argument.0, len))
            })
            .collect()
    };
    unsafe {
        let _ = LocalFree(HLOCAL(argv.cast::<core::ffi::c_void>()));
    }
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_paths_and_windows_arguments_are_owned() {
        let mut path: Vec<u16> = r"C:\Temp\école\target.exe".encode_utf16().collect();
        path.push(0);
        assert_eq!(
            path_from_buffer(&path),
            Some(PathBuf::from(r"C:\Temp\école\target.exe"))
        );

        let mut arguments: Vec<u16> = r#"--name "two words" --flag"#.encode_utf16().collect();
        arguments.push(0);
        assert_eq!(
            parse_arguments(&arguments),
            ["--name", "two words", "--flag"]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
    }
}
