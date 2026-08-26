//! WSL roots behind Ferail's path-backed platform-location capability.
//!
//! The host-independent state machine and UI live in `ferail-core` / GPUI.
//! This module owns Windows discovery and activation. Pure path/output parsing
//! is compiled on every host so it can be tested from the macOS development
//! machine; registry and process mechanics remain `cfg(windows)`.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ferail_core::platform_locations::{
    PathBackedPlatformRoot, PlatformRootErrorKind, PlatformRootId,
};

#[cfg(not(windows))]
pub fn discover_path_backed_platform_roots(_cancel: &AtomicBool) -> Vec<PathBackedPlatformRoot> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn activate_path_backed_platform_root(
    _id: &PlatformRootId,
    _cancel: &AtomicBool,
) -> Result<PathBuf, PlatformRootErrorKind> {
    Err(PlatformRootErrorKind::Unavailable)
}

/// Prefer the current stable UNC authority for paths Ferail creates. Parsing
/// accepts the legacy `wsl$` alias as well.
pub(crate) fn distro_unc_path(name: &str) -> Option<PathBuf> {
    if name.trim() != name
        || name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['\\', '/'])
        || name.chars().any(char::is_control)
    {
        return None;
    }
    Some(PathBuf::from(format!(r"\\wsl.localhost\{name}")))
}

/// Parse a WSL UNC path without touching the filesystem. Returns the authority,
/// distribution and absolute Linux path while preserving component case.
pub(crate) fn parse_wsl_unc(path: &Path) -> Option<(String, String, String)> {
    let raw = path.to_string_lossy().replace('/', "\\");
    let plain = strip_extended_unc(&raw)?;
    let mut parts = plain.split('\\').filter(|part| !part.is_empty());
    let authority = parts.next()?;
    if !authority.eq_ignore_ascii_case("wsl$") && !authority.eq_ignore_ascii_case("wsl.localhost") {
        return None;
    }
    let distro = parts.next()?;
    distro_unc_path(distro)?;
    let tail: Vec<&str> = parts.collect();
    if tail.iter().any(|part| matches!(*part, "." | "..")) {
        return None;
    }
    let linux_path = if tail.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", tail.join("/"))
    };
    Some((authority.to_string(), distro.to_string(), linux_path))
}

pub fn is_wsl_path(path: &Path) -> bool {
    parse_wsl_unc(path).is_some()
}

fn strip_extended_unc(raw: &str) -> Option<&str> {
    if raw.len() >= 8 && raw[..8].eq_ignore_ascii_case(r"\\?\UNC\") {
        Some(&raw[8..])
    } else if let Some(plain) = raw.strip_prefix(r"\\") {
        Some(plain)
    } else {
        None
    }
}

/// `wsl.exe --list --quiet` has emitted both console UTF-16LE and UTF-8 across
/// Windows/WSL releases. Decode either form without retaining command output.
#[cfg_attr(not(windows), allow(dead_code))]
fn decode_wsl_lines(bytes: &[u8]) -> Vec<String> {
    let looks_utf16 = bytes.starts_with(&[0xff, 0xfe])
        || bytes
            .chunks_exact(2)
            .take(32)
            .filter(|pair| pair[1] == 0)
            .count()
            >= 3;
    let text = if looks_utf16 {
        let start = usize::from(bytes.starts_with(&[0xff, 0xfe])) * 2;
        let units: Vec<u16> = bytes[start..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.lines()
        .map(|line| line.trim_matches(['\0', '\r', '\n', ' ']))
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(windows)]
mod windows_impl {
    use std::collections::HashSet;
    use std::io::Read as _;
    use std::os::windows::process::CommandExt as _;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_PATH_NOT_FOUND,
        ERROR_SUCCESS,
    };
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_ENUMERATE_SUB_KEYS, KEY_READ, REG_VALUE_TYPE,
    };
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;

    use super::*;
    use ferail_core::platform_locations::PathBackedRootState;

    const LXSS: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";
    const LIST_TIMEOUT: Duration = Duration::from_secs(3);
    const START_TIMEOUT: Duration = Duration::from_secs(20);
    const SYMLINK_TIMEOUT: Duration = Duration::from_secs(5);
    const OUTPUT_CAPTURE_LIMIT: usize = 1024 * 1024;

    #[derive(Debug)]
    struct Distro {
        id: String,
        name: String,
        version: Option<u32>,
        is_default: bool,
    }

    pub(super) fn discover(cancel: &AtomicBool) -> Vec<PathBackedPlatformRoot> {
        if cancel.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let mut distros = enumerate_registry_distros();
        let running: HashSet<String> =
            run_wsl(&["--list", "--running", "--quiet"], LIST_TIMEOUT, cancel)
                .ok()
                .map(|bytes| {
                    decode_wsl_lines(&bytes)
                        .into_iter()
                        .map(|name| name.to_lowercase())
                        .collect()
                })
                .unwrap_or_default();

        // Modern Store/inbox WSL can exist without the predecessor's MSI
        // marker. If the per-user registry yielded nothing, ask WSL itself.
        if distros.is_empty() {
            if let Ok(bytes) = run_wsl(&["--list", "--quiet"], LIST_TIMEOUT, cancel) {
                distros = decode_wsl_lines(&bytes)
                    .into_iter()
                    .map(|name| Distro {
                        id: format!("name:{name}"),
                        name,
                        version: None,
                        is_default: false,
                    })
                    .collect();
            }
        }

        distros
            .into_iter()
            .filter_map(|distro| {
                let path = distro_unc_path(&distro.name)?;
                let state = if running.contains(&distro.name.to_lowercase()) {
                    PathBackedRootState::Ready(path)
                } else {
                    PathBackedRootState::Stopped
                };
                Some(PathBackedPlatformRoot {
                    id: PlatformRootId::new(distro.id),
                    label: distro.name.into(),
                    state,
                    version: distro.version,
                    is_default: distro.is_default,
                })
            })
            .collect()
    }

    pub(super) fn activate(
        id: &PlatformRootId,
        cancel: &AtomicBool,
    ) -> Result<PathBuf, PlatformRootErrorKind> {
        let key = id.as_provider_key();
        let name = if let Some(name) = key.strip_prefix("name:") {
            name.to_string()
        } else {
            enumerate_registry_distros()
                .into_iter()
                .find(|distro| distro.id.eq_ignore_ascii_case(key))
                .map(|distro| distro.name)
                .ok_or(PlatformRootErrorKind::NotFound)?
        };
        let path = distro_unc_path(&name).ok_or(PlatformRootErrorKind::Failed)?;
        run_wsl(&["-d", &name, "--exec", "/bin/true"], START_TIMEOUT, cancel)?;
        Ok(path)
    }

    pub(super) fn resolve_symlink(
        path: &Path,
        cancel: &AtomicBool,
    ) -> Result<PathBuf, PlatformRootErrorKind> {
        let (authority, distro, linux_path) =
            parse_wsl_unc(path).ok_or(PlatformRootErrorKind::NotFound)?;
        let bytes = run_wsl(
            &["-d", &distro, "--exec", "readlink", "-f", "--", &linux_path],
            SYMLINK_TIMEOUT,
            cancel,
        )?;
        let resolved = decode_wsl_lines(&bytes)
            .into_iter()
            .next()
            .ok_or(PlatformRootErrorKind::NotFound)?;
        resolved_linux_to_windows(&authority, &distro, &resolved)
            .ok_or(PlatformRootErrorKind::Failed)
    }

    fn run_wsl(
        args: &[&str],
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, PlatformRootErrorKind> {
        if cancel.load(Ordering::Relaxed) {
            return Err(PlatformRootErrorKind::Cancelled);
        }
        let mut child = Command::new("wsl.exe")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW.0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| PlatformRootErrorKind::Unavailable)?;
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PlatformRootErrorKind::Failed);
        };
        // Drain concurrently so a verbose or corrupted helper cannot fill the
        // OS pipe and prevent the child from exiting. Retain at most the small
        // bounded payload the parsers need; excess bytes are discarded.
        let output_reader = std::thread::spawn(move || {
            let mut captured = Vec::new();
            let mut overflowed = false;
            let mut buffer = [0u8; 8192];
            while let Ok(read) = stdout.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                let remaining = OUTPUT_CAPTURE_LIMIT.saturating_sub(captured.len());
                captured.extend_from_slice(&buffer[..read.min(remaining)]);
                overflowed |= read > remaining;
            }
            (captured, overflowed)
        });
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = output_reader.join();
                return Err(PlatformRootErrorKind::Cancelled);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    let (bytes, overflowed) = output_reader.join().unwrap_or_default();
                    return if status.success() && !overflowed {
                        Ok(bytes)
                    } else {
                        Err(PlatformRootErrorKind::Failed)
                    };
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = output_reader.join();
                    return Err(PlatformRootErrorKind::TimedOut);
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = output_reader.join();
                    return Err(PlatformRootErrorKind::Failed);
                }
            }
        }
    }

    fn enumerate_registry_distros() -> Vec<Distro> {
        unsafe { enumerate_registry_distros_inner().unwrap_or_default() }
    }

    unsafe fn enumerate_registry_distros_inner() -> Result<Vec<Distro>, u32> {
        let subkey = wide(LXSS);
        let mut root = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(subkey.as_ptr()),
            0,
            KEY_READ | KEY_ENUMERATE_SUB_KEYS,
            &mut root,
        );
        if result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND {
            return Ok(Vec::new());
        }
        if result != ERROR_SUCCESS {
            return Err(result.0);
        }
        let default_id = read_string(root, "DefaultDistribution").unwrap_or_default();
        let mut distros = Vec::new();
        let mut index = 0;
        loop {
            let mut name = vec![0u16; 512];
            let mut len = name.len() as u32;
            let status = RegEnumKeyExW(
                root,
                index,
                PWSTR(name.as_mut_ptr()),
                &mut len,
                None,
                PWSTR::null(),
                None,
                None,
            );
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            index += 1;
            if status != ERROR_SUCCESS {
                continue;
            }
            let id = String::from_utf16_lossy(&name[..len as usize]);
            if !id.starts_with('{') {
                continue;
            }
            let child_name = wide(&format!(r"{LXSS}\{id}"));
            let mut child = HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR::from_raw(child_name.as_ptr()),
                0,
                KEY_READ,
                &mut child,
            ) != ERROR_SUCCESS
            {
                continue;
            }
            if let Some(name) = read_string(child, "DistributionName") {
                distros.push(Distro {
                    id: id.clone(),
                    name,
                    version: read_dword(child, "Version"),
                    is_default: id.eq_ignore_ascii_case(&default_id),
                });
            }
            let _ = RegCloseKey(child);
        }
        let _ = RegCloseKey(root);
        Ok(distros)
    }

    unsafe fn read_string(key: HKEY, name: &str) -> Option<String> {
        let name = wide(name);
        let mut kind = REG_VALUE_TYPE::default();
        let mut bytes = 0;
        let status = RegQueryValueExW(
            key,
            PCWSTR::from_raw(name.as_ptr()),
            None,
            Some(&mut kind),
            None,
            Some(&mut bytes),
        );
        if (status != ERROR_SUCCESS && status != ERROR_MORE_DATA) || bytes < 2 {
            return None;
        }
        let mut value = vec![0u16; bytes as usize / 2 + 1];
        let mut capacity = (value.len() * 2) as u32;
        if RegQueryValueExW(
            key,
            PCWSTR::from_raw(name.as_ptr()),
            None,
            Some(&mut kind),
            Some(value.as_mut_ptr().cast()),
            Some(&mut capacity),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let len = value
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(value.len());
        let value = String::from_utf16_lossy(&value[..len]).trim().to_string();
        (!value.is_empty()).then_some(value)
    }

    unsafe fn read_dword(key: HKEY, name: &str) -> Option<u32> {
        let name = wide(name);
        let mut kind = REG_VALUE_TYPE::default();
        let mut value = 0u32;
        let mut bytes = size_of::<u32>() as u32;
        (RegQueryValueExW(
            key,
            PCWSTR::from_raw(name.as_ptr()),
            None,
            Some(&mut kind),
            Some((&mut value as *mut u32).cast()),
            Some(&mut bytes),
        ) == ERROR_SUCCESS)
            .then_some(value)
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain([0]).collect()
    }
}

fn resolved_linux_to_windows(authority: &str, distro: &str, path: &str) -> Option<PathBuf> {
    if !path.starts_with('/') || path.contains(['\0', '\r', '\n']) {
        return None;
    }
    let components: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if components.iter().any(|part| matches!(*part, "." | "..")) {
        return None;
    }
    if components.len() >= 2
        && components[0] == "mnt"
        && components[1].len() == 1
        && components[1].as_bytes()[0].is_ascii_alphabetic()
    {
        let drive = (components[1].as_bytes()[0] as char).to_ascii_uppercase();
        let tail = components[2..].join("\\");
        return Some(if tail.is_empty() {
            PathBuf::from(format!(r"{drive}:\"))
        } else {
            PathBuf::from(format!(r"{drive}:\{tail}"))
        });
    }
    let tail = components.join("\\");
    let root = format!(r"\\{authority}\{distro}");
    Some(if tail.is_empty() {
        PathBuf::from(root)
    } else {
        PathBuf::from(format!(r"{root}\{tail}"))
    })
}

#[cfg(windows)]
pub fn discover_path_backed_platform_roots(cancel: &AtomicBool) -> Vec<PathBackedPlatformRoot> {
    windows_impl::discover(cancel)
}

#[cfg(windows)]
pub fn activate_path_backed_platform_root(
    id: &PlatformRootId,
    cancel: &AtomicBool,
) -> Result<PathBuf, PlatformRootErrorKind> {
    windows_impl::activate(id, cancel)
}

#[cfg(windows)]
pub fn resolve_wsl_symlink_path(
    path: &Path,
    cancel: &AtomicBool,
) -> Result<PathBuf, PlatformRootErrorKind> {
    windows_impl::resolve_symlink(path, cancel)
}

#[cfg(not(windows))]
pub fn resolve_wsl_symlink_path(
    _path: &Path,
    _cancel: &AtomicBool,
) -> Result<PathBuf, PlatformRootErrorKind> {
    Err(PlatformRootErrorKind::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_path_rejects_separators_and_dot_names() {
        assert_eq!(
            distro_unc_path("Ubuntu"),
            Some(PathBuf::from(r"\\wsl.localhost\Ubuntu"))
        );
        assert_eq!(distro_unc_path("Ubuntu/evil"), None);
        assert_eq!(distro_unc_path(" Ubuntu"), None);
        assert_eq!(distro_unc_path("Ubuntu\n"), None);
        assert_eq!(distro_unc_path(".."), None);
        assert_eq!(distro_unc_path(""), None);
    }

    #[test]
    fn parses_both_wsl_authorities_and_extended_unc() {
        assert_eq!(
            parse_wsl_unc(Path::new(r"\\wsl.localhost\Ubuntu\home\Zoë")),
            Some(("wsl.localhost".into(), "Ubuntu".into(), "/home/Zoë".into()))
        );
        assert_eq!(
            parse_wsl_unc(Path::new(r"\\wsl$\My Distro")),
            Some(("wsl$".into(), "My Distro".into(), "/".into()))
        );
        assert_eq!(
            parse_wsl_unc(Path::new(r"\\?\UNC\wsl$\Ubuntu\bin")),
            Some(("wsl$".into(), "Ubuntu".into(), "/bin".into()))
        );
        assert_eq!(parse_wsl_unc(Path::new(r"\\server\share")), None);
        assert_eq!(parse_wsl_unc(Path::new(r"\\wsl$\Ubuntu\home\..\etc")), None);
    }

    #[test]
    fn decodes_utf8_and_utf16_wsl_output() {
        assert_eq!(
            decode_wsl_lines(b"Ubuntu\r\nAlpine\r\n"),
            ["Ubuntu", "Alpine"]
        );
        let units: Vec<u16> = "Ubuntu\r\nDebian\r\n".encode_utf16().collect();
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(units.into_iter().flat_map(u16::to_le_bytes));
        assert_eq!(decode_wsl_lines(&bytes), ["Ubuntu", "Debian"]);
    }

    #[test]
    fn resolved_linux_paths_preserve_authority_and_convert_drive_mounts() {
        assert_eq!(
            resolved_linux_to_windows("wsl.localhost", "Ubuntu", "/usr/bin"),
            Some(PathBuf::from(r"\\wsl.localhost\Ubuntu\usr\bin"))
        );
        assert_eq!(
            resolved_linux_to_windows("wsl$", "Ubuntu", "/mnt/c/Users/Test"),
            Some(PathBuf::from(r"C:\Users\Test"))
        );
        assert_eq!(
            resolved_linux_to_windows("wsl$", "Ubuntu", "relative"),
            None
        );
        assert_eq!(
            resolved_linux_to_windows("wsl$", "Ubuntu", "/home/../etc"),
            None
        );
    }
}
