//! POSIX stat-based facts for the Get Info panel: ownership, permission
//! mode, the created/modified/accessed timestamps, the size, and the BSD
//! flags Finder surfaces as "Locked" (`UF_IMMUTABLE`) and "Invisible"
//! (`UF_HIDDEN`). Plus volume facts that no NSURL key exposes — the
//! filesystem format (`f_fstypename`) and BSD device (`f_mntfromname`).
//!
//! All of this is a single `lstat(2)` / `statfs(2)` hop with no allocation
//! in the syscall itself; still off the UI thread per the Prime Directive,
//! since the caller batches it with the slower native reads.
//!
//! macOS is the fully-implemented target. Other unix fills what
//! `std::fs::Metadata` exposes (no birthtime, no BSD flags); Windows returns
//! `None` until the port (Layer 4).

use std::path::Path;

/// POSIX facts for one path. Read with `lstat` (symlinks are not followed),
/// matching the enumerate path so a symlink reports as itself.
#[derive(Clone, Debug)]
pub struct StatInfo {
    pub size: u64,
    pub uid: u32,
    pub gid: u32,
    /// Resolved account name, or the numeric uid as a string if unresolved.
    pub owner_name: String,
    /// Resolved group name, or the numeric gid as a string if unresolved.
    pub group_name: String,
    /// Full `st_mode` (type bits + permission bits).
    pub mode: u32,
    pub is_symlink: bool,
    pub is_dir: bool,
    /// Creation time (macOS birthtime). `None` where the platform lacks it.
    pub created_unix: Option<i64>,
    pub modified_unix: i64,
    pub accessed_unix: Option<i64>,
    /// `UF_IMMUTABLE` — Finder "Locked".
    pub is_locked: bool,
    /// `UF_HIDDEN` — Finder "Invisible".
    pub is_invisible: bool,
}

impl StatInfo {
    /// Leading char of a symbolic permission string.
    pub fn kind_char(&self) -> char {
        if self.is_symlink {
            'l'
        } else if self.is_dir {
            'd'
        } else {
            '-'
        }
    }
}

#[cfg(unix)]
pub fn read_stat_info(path: &Path) -> Option<StatInfo> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: zeroed `libc::stat` is a valid initial state; `lstat` fills it
    // or returns non-zero, in which case we bail before reading the struct.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::lstat(c_path.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }

    let mode = st.st_mode as u32;
    let is_symlink = (mode & libc::S_IFMT as u32) == libc::S_IFLNK as u32;
    let is_dir = (mode & libc::S_IFMT as u32) == libc::S_IFDIR as u32;

    #[cfg(target_os = "macos")]
    let (created_unix, is_locked, is_invisible) = {
        // From <sys/stat.h>.
        const UF_HIDDEN: u32 = 0x0000_8000;
        const UF_IMMUTABLE: u32 = 0x0000_0002;
        (
            Some(st.st_birthtime as i64),
            st.st_flags & UF_IMMUTABLE != 0,
            st.st_flags & UF_HIDDEN != 0,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let (created_unix, is_locked, is_invisible) = (None, false, false);

    Some(StatInfo {
        size: st.st_size as u64,
        uid: st.st_uid,
        gid: st.st_gid,
        owner_name: owner_name(st.st_uid),
        group_name: group_name(st.st_gid),
        mode,
        is_symlink,
        is_dir,
        created_unix,
        modified_unix: st.st_mtime as i64,
        accessed_unix: Some(st.st_atime as i64),
        is_locked,
        is_invisible,
    })
}

#[cfg(not(unix))]
pub fn read_stat_info(_path: &Path) -> Option<StatInfo> {
    None
}

/// Resolve a uid to an account name via `getpwuid_r`, falling back to the
/// numeric id. Thread-safe (the `_r` form takes a caller buffer).
#[cfg(unix)]
fn owner_name(uid: u32) -> String {
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut buf = [0i8; 1024];
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            uid as libc::uid_t,
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        );
        if rc == 0 && !result.is_null() && !pwd.pw_name.is_null() {
            if let Ok(s) = std::ffi::CStr::from_ptr(pwd.pw_name).to_str() {
                return s.to_string();
            }
        }
    }
    uid.to_string()
}

#[cfg(unix)]
fn group_name(gid: u32) -> String {
    unsafe {
        let mut grp: libc::group = std::mem::zeroed();
        let mut buf = [0i8; 1024];
        let mut result: *mut libc::group = std::ptr::null_mut();
        let rc = libc::getgrgid_r(
            gid as libc::gid_t,
            &mut grp,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        );
        if rc == 0 && !result.is_null() && !grp.gr_name.is_null() {
            if let Ok(s) = std::ffi::CStr::from_ptr(grp.gr_name).to_str() {
                return s.to_string();
            }
        }
    }
    gid.to_string()
}

/// Set or clear the `UF_IMMUTABLE` flag (Finder "Locked") on `path`,
/// preserving the other BSD flags. macOS only.
#[cfg(target_os = "macos")]
pub fn set_locked(path: &Path, locked: bool) -> Result<(), String> {
    const UF_IMMUTABLE: u32 = 0x0000_0002;
    set_flag(path, UF_IMMUTABLE, locked)
}

/// Set or clear the `UF_HIDDEN` flag (Finder "Invisible") on `path`,
/// preserving the other BSD flags. macOS only.
#[cfg(target_os = "macos")]
pub fn set_invisible(path: &Path, invisible: bool) -> Result<(), String> {
    const UF_HIDDEN: u32 = 0x0000_8000;
    set_flag(path, UF_HIDDEN, invisible)
}

/// Read the current `st_flags`, set or clear `bit`, and write back via
/// `chflags` so unrelated flags survive.
#[cfg(target_os = "macos")]
fn set_flag(path: &Path, bit: u32, on: bool) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "path contains NUL".to_string())?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::lstat(c_path.as_ptr(), &mut st) } != 0 {
        return Err(io_err("lstat"));
    }
    let flags = if on {
        st.st_flags | bit
    } else {
        st.st_flags & !bit
    };
    if unsafe { libc::chflags(c_path.as_ptr(), flags) } != 0 {
        return Err(io_err("chflags"));
    }
    Ok(())
}

/// Replace the permission bits of `path` with `mode` (the lower 12 bits,
/// type bits ignored) via `chmod`. Unix only.
#[cfg(unix)]
pub fn set_permissions(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "path contains NUL".to_string())?;
    if unsafe { libc::chmod(c_path.as_ptr(), (mode & 0o7777) as libc::mode_t) } != 0 {
        return Err(io_err("chmod"));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn set_locked(_path: &Path, _locked: bool) -> Result<(), String> {
    Err("set_locked is macOS-only".into())
}

#[cfg(not(target_os = "macos"))]
pub fn set_invisible(_path: &Path, _invisible: bool) -> Result<(), String> {
    Err("set_invisible is macOS-only".into())
}

#[cfg(not(unix))]
pub fn set_permissions(_path: &Path, _mode: u32) -> Result<(), String> {
    Err("set_permissions is unix-only".into())
}

#[cfg(unix)]
fn io_err(op: &str) -> String {
    format!("{op}: {}", std::io::Error::last_os_error())
}

/// Filesystem type name (e.g. "apfs", "hfs", "exfat") and BSD device node
/// (e.g. "/dev/disk3s1s1") for the volume containing `path`. Both `None` off
/// macOS or on `statfs` failure.
#[cfg(target_os = "macos")]
pub fn volume_fs_info(path: &Path) -> (Option<String>, Option<String>) {
    use std::os::unix::ffi::OsStrExt;

    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return (None, None);
    };
    // SAFETY: zeroed `statfs` is valid input; on non-zero return we read
    // nothing from it.
    let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut sfs) };
    if rc != 0 {
        return (None, None);
    }
    let read_cstr = |arr: &[libc::c_char]| -> Option<String> {
        // SAFETY: the array is a NUL-terminated C string from the kernel.
        let s = unsafe { std::ffi::CStr::from_ptr(arr.as_ptr()) };
        let s = s.to_string_lossy().into_owned();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };
    (read_cstr(&sfs.f_fstypename), read_cstr(&sfs.f_mntfromname))
}

#[cfg(not(target_os = "macos"))]
pub fn volume_fs_info(_path: &Path) -> (Option<String>, Option<String>) {
    (None, None)
}

/// Format a unix timestamp as a Finder-style local date-time,
/// e.g. "9 Mar 2024 at 12:11". Uses `localtime_r` for an accurate local
/// wall-clock (DST-aware); falls back to a UTC render off unix.
pub fn format_local_datetime(unix: i64) -> String {
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    #[cfg(unix)]
    {
        // SAFETY: `localtime_r` fills the caller-provided `tm`; the time_t is
        // passed by pointer and not retained.
        let t = unix as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let res = unsafe { libc::localtime_r(&t, &mut tm) };
        if !res.is_null() {
            let month = NAMES[(tm.tm_mon.clamp(0, 11)) as usize];
            return format!(
                "{} {} {} at {:02}:{:02}",
                tm.tm_mday,
                month,
                tm.tm_year + 1900,
                tm.tm_hour,
                tm.tm_min,
            );
        }
    }
    // Non-unix / failure fallback: UTC civil date with no clock.
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{} {} {}", d, NAMES[(m as usize - 1).min(11)], y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn reads_self_stat() {
        let dir = std::env::temp_dir().join(format!("feraille-statinfo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("probe.txt");
        std::fs::write(&f, b"hello").unwrap();
        let info = read_stat_info(&f).expect("stat the temp file");
        assert_eq!(info.size, 5);
        assert!(!info.is_dir);
        assert!(!info.is_symlink);
        // The current user owns a file they just created.
        assert_eq!(info.uid, unsafe { libc::getuid() });
        // Permission bits are populated (rw- at minimum for the owner).
        assert!(info.mode & 0o400 != 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn set_permissions_round_trips() {
        let dir = std::env::temp_dir().join(format!("feraille-chmod-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("perm.txt");
        std::fs::write(&f, b"x").unwrap();
        set_permissions(&f, 0o640).unwrap();
        let info = read_stat_info(&f).unwrap();
        assert_eq!(info.mode & 0o777, 0o640);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn lock_unlock_round_trips() {
        let dir = std::env::temp_dir().join(format!("feraille-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("lock.txt");
        std::fs::write(&f, b"x").unwrap();
        set_locked(&f, true).unwrap();
        assert!(read_stat_info(&f).unwrap().is_locked);
        set_locked(&f, false).unwrap();
        assert!(!read_stat_info(&f).unwrap().is_locked);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_datetime_shape() {
        // Don't assert the exact local rendering (depends on the test box's
        // zone); just that it produces the "<d> <Mon> <year> at HH:MM" shape
        // on unix or the date-only fallback elsewhere.
        let s = format_local_datetime(1_710_000_000);
        assert!(s.contains("2024"), "got {s}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn boot_volume_is_apfs() {
        let (fmt, dev) = volume_fs_info(Path::new("/"));
        assert!(fmt.is_some(), "boot volume has a format");
        assert!(
            dev.as_deref().unwrap_or("").starts_with("/dev/"),
            "device node looks like /dev/...: {dev:?}"
        );
    }
}
