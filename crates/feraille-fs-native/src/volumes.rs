//! Volume / drive enumeration.
//!
//! macOS reads `/Volumes` and resolves NSURL keys via shell-mac;
//! Windows enumerates drive letters via `GetLogicalDrives` and
//! classifies / measures each one via `GetDriveTypeW` +
//! `GetVolumeInformationW` + `GetDiskFreeSpaceExW`.
//!
//! v1 is pure synchronous Win32 — fine because drive enumeration is
//! a sidebar refresh operation, not a per-frame call. Per the prime
//! directive, callers must not invoke this from the paint path.

use std::path::Path;

use crate::VolumeInfo;

#[cfg(windows)]
pub fn list_volumes() -> Vec<VolumeInfo> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };

    // GetDriveTypeW return codes. Hardcoded because the `windows`
    // crate 0.58 doesn't re-export these constants — they've been
    // stable since Win9x.
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;
    const DRIVE_RAMDISK: u32 = 6;

    let mut out: Vec<VolumeInfo> = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return out;
    }

    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        // Drive root, e.g. "C:\\". Win32 APIs accept a UTF-16 null-
        // terminated string; we use a Vec<u16> for the buffer.
        let root_wide: Vec<u16> = format!("{letter}:\\\0").encode_utf16().collect();
        let root_pcwstr = PCWSTR::from_raw(root_wide.as_ptr());

        let kind = unsafe { GetDriveTypeW(root_pcwstr) };
        // Skip A: / B: floppy slots and unknown — they appear in the
        // bitmask even when no media is inserted and probing them
        // spins up the drive or blocks. Keep CD/DVD, removable USB,
        // fixed disks, network shares, and RAM disks.
        let (is_local, is_removable) = match kind {
            DRIVE_FIXED => (true, false),
            DRIVE_REMOVABLE => (true, true),
            DRIVE_CDROM => (true, true),
            DRIVE_RAMDISK => (true, false),
            DRIVE_REMOTE => (false, false),
            _ => continue,
        };

        // Volume label. Buffer sized per MSDN (MAX_PATH+1 for the
        // volume name; we don't read the FS name or serial).
        let mut name_buf = [0u16; 261];
        let name = unsafe {
            match GetVolumeInformationW(
                root_pcwstr,
                Some(&mut name_buf),
                None,
                None,
                None,
                None,
            ) {
                Ok(()) => {
                    let len = name_buf.iter().position(|&c| c == 0).unwrap_or(name_buf.len());
                    let label = String::from_utf16_lossy(&name_buf[..len]);
                    if label.is_empty() {
                        format!("{letter}:")
                    } else {
                        format!("{label} ({letter}:)")
                    }
                }
                Err(_) => format!("{letter}:"),
            }
        };

        // Capacity. Only ask for it on local drives — network shares
        // can issue a remote round-trip even for "cheap" queries.
        let (total_bytes, available_bytes) = if is_local {
            let mut free_to_caller: u64 = 0;
            let mut total: u64 = 0;
            let mut total_free: u64 = 0;
            unsafe {
                match GetDiskFreeSpaceExW(
                    root_pcwstr,
                    Some(&mut free_to_caller),
                    Some(&mut total),
                    Some(&mut total_free),
                ) {
                    Ok(()) => (Some(total), Some(free_to_caller)),
                    Err(_) => (None, None),
                }
            }
        } else {
            (None, None)
        };

        out.push(VolumeInfo {
            path: format!("{letter}:\\").into(),
            name,
            total_bytes,
            available_bytes,
            is_local,
            is_removable,
        });
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

#[cfg(target_os = "macos")]
pub fn list_volumes() -> Vec<VolumeInfo> {
    let mut out: Vec<VolumeInfo> = Vec::new();
    let Ok(read_dir) = std::fs::read_dir("/Volumes") else {
        return out;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        let info = crate::volume_info_for_path(&path).unwrap_or_else(|| VolumeInfo {
            name: path
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| path.display().to_string()),
            path: path.clone(),
            total_bytes: None,
            available_bytes: None,
            is_local: true,
            is_removable: false,
        });
        out.push(info);
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn list_volumes() -> Vec<VolumeInfo> {
    Vec::new()
}

#[cfg(windows)]
pub fn volume_info_for_path(path: &Path) -> Option<VolumeInfo> {
    // For now, Win32 volume lookup goes through list_volumes; per-path
    // lookup isn't called on Windows code paths today. Returning None
    // keeps the signature compatible with shell-mac.
    let _ = path;
    None
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn volume_info_for_path(_path: &Path) -> Option<VolumeInfo> {
    None
}
