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

// Only the windows + fallback arms of `volume_info_for_path` take
// &Path here; the macOS arm lives in lib.rs. cfg the import so the
// mac build doesn't warn.
#[cfg(not(target_os = "macos"))]
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
        // volume name; we don't read the FS name or serial). Skipped
        // for network drives: GetVolumeInformationW on a mapped drive
        // whose server is unreachable blocks for the full SMB/DFS
        // timeout (tens of seconds) — the bare letter is an honest
        // label, and the sidebar refresh picks up a nicer one when the
        // share is actually reachable via the capacity-free path.
        let name = if kind == DRIVE_REMOTE {
            format!("{letter}:")
        } else {
            let mut name_buf = [0u16; 261];
            unsafe {
                match GetVolumeInformationW(
                    root_pcwstr,
                    Some(&mut name_buf),
                    None,
                    None,
                    None,
                    None,
                ) {
                    Ok(()) => {
                        let len =
                            name_buf.iter().position(|&c| c == 0).unwrap_or(name_buf.len());
                        let label = String::from_utf16_lossy(&name_buf[..len]);
                        if label.is_empty() {
                            format!("{letter}:")
                        } else {
                            format!("{label} ({letter}:)")
                        }
                    }
                    Err(_) => format!("{letter}:"),
                }
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
            format: None,
            bsd_device: None,
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
            format: None,
            bsd_device: None,
        });
        out.push(info);
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// Linux volumes from `/proc/self/mountinfo`: the root filesystem, `/home`,
/// and removable / user mounts (`/media`, `/run/media`, `/mnt`) that sit on a
/// real block device — skipping the pile of pseudo/virtual filesystems. Sizes
/// come from `statvfs`. Mirrors the macOS `/Volumes` enumeration for the
/// sidebar's Volumes section.
#[cfg(target_os = "linux")]
pub fn list_volumes() -> Vec<VolumeInfo> {
    use std::collections::HashSet;
    use std::io::{BufRead, BufReader};

    // Virtual / pseudo filesystems that are never user "volumes".
    const PSEUDO_FS: &[&str] = &[
        "proc", "sysfs", "tmpfs", "devtmpfs", "devpts", "cgroup", "cgroup2", "mqueue",
        "hugetlbfs", "debugfs", "tracefs", "securityfs", "pstore", "bpf", "configfs",
        "fusectl", "binfmt_misc", "autofs", "ramfs", "rpc_pipefs", "nsfs", "overlay",
        "squashfs", "efivarfs", "selinuxfs", "fuse.gvfsd-fuse", "fuse.portal",
    ];

    let Ok(file) = std::fs::File::open("/proc/self/mountinfo") else {
        return Vec::new();
    };

    let mut out: Vec<VolumeInfo> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        // mountinfo: <pre fields...> - <fs_type> <source> <super opts>
        let Some((pre, post)) = line.split_once(" - ") else {
            continue;
        };
        let pre: Vec<&str> = pre.split_whitespace().collect();
        let post: Vec<&str> = post.split_whitespace().collect();
        if pre.len() < 5 || post.len() < 2 {
            continue;
        }
        let mount_point = unescape_mountinfo(pre[4]);
        let fs_type = post[0];
        let source = post[1];

        if PSEUDO_FS.contains(&fs_type) || seen.contains(&mount_point) {
            continue;
        }
        let is_block = source.starts_with("/dev/");
        let is_user_mount = mount_point.starts_with("/media/")
            || mount_point.starts_with("/run/media/")
            || mount_point.starts_with("/mnt/");
        let keep = (mount_point == "/" || mount_point == "/home" || is_user_mount) && is_block;
        if !keep {
            continue;
        }
        seen.insert(mount_point.clone());

        let (total, available) = statvfs_bytes(&mount_point);
        let name = if mount_point == "/" {
            "Root".to_string()
        } else {
            std::path::Path::new(&mount_point)
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| mount_point.clone())
        };
        out.push(VolumeInfo {
            name,
            path: std::path::PathBuf::from(&mount_point),
            total_bytes: total,
            available_bytes: available,
            is_local: true,
            is_removable: is_user_mount,
            format: Some(fs_type.to_string()),
            bsd_device: Some(source.to_string()),
        });
    }

    // Root first, then alphabetical.
    out.sort_by(|a, b| {
        let ar = a.path == std::path::Path::new("/");
        let br = b.path == std::path::Path::new("/");
        br.cmp(&ar).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// Decode the octal escapes `/proc/self/mountinfo` uses for space (`\040`),
/// tab (`\011`), newline (`\012`) and backslash (`\134`) in path fields.
#[cfg(target_os = "linux")]
fn unescape_mountinfo(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let oct = &s[i + 1..i + 4];
            if let Ok(n) = u8::from_str_radix(oct, 8) {
                out.push(n as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Total + available bytes for the filesystem at `path` via `statvfs`.
#[cfg(target_os = "linux")]
fn statvfs_bytes(path: &str) -> (Option<u64>, Option<u64>) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(std::ffi::OsStr::new(path).as_bytes()) else {
        return (None, None);
    };
    // SAFETY: zeroed statvfs is a valid initial state; on non-zero return we
    // read nothing from it.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut st) } != 0 {
        return (None, None);
    }
    let frsize = st.f_frsize as u64;
    let total = (st.f_blocks as u64).checked_mul(frsize);
    let avail = (st.f_bavail as u64).checked_mul(frsize);
    (total, avail)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
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
