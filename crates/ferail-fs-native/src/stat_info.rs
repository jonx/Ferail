//! POSIX stat-based facts for the Get Info panel: ownership, permission
//! mode, the created/modified/accessed timestamps, the size, and the BSD
//! flags Finder surfaces as "Locked" (`UF_IMMUTABLE`) and "Invisible"
//! (`UF_HIDDEN`). Plus volume facts that no NSURL key exposes: the
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

use ferail_core::entry_info::TimestampKind;

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
    /// `UF_IMMUTABLE`: Finder "Locked".
    pub is_locked: bool,
    /// `UF_HIDDEN`: Finder "Invisible".
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
    // `mode_t` is u16 on macOS and u32 on Linux: the casts are load-bearing
    // on one and a no-op on the other, so clippy's same-type lint is wrong
    // on exactly one platform.
    #[allow(clippy::unnecessary_cast)]
    let is_symlink = (mode & libc::S_IFMT as u32) == libc::S_IFLNK as u32;
    #[allow(clippy::unnecessary_cast)]
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

/// Windows facts for the Get Info panel, the rough analogue of the unix
/// `lstat` read. `std`'s `MetadataExt` exposes everything the Properties
/// dialog's General tab shows: size, the created/modified/accessed
/// timestamps, and the read-only / hidden attribute bits, with no `windows`
/// crate needed. There is no POSIX uid/gid/mode, so a permission mode is
/// synthesised from the read-only attribute (write bits cleared when set) and
/// the owner shows the current account (best-effort).
#[cfg(windows)]
pub fn read_stat_info(path: &Path) -> Option<StatInfo> {
    use std::os::windows::fs::MetadataExt;

    // symlink_metadata so a symlink / junction reports as itself, matching the
    // unix lstat path and the directory enumerator.
    let md = std::fs::symlink_metadata(path).ok()?;
    let attrs = md.file_attributes();

    const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
    let is_symlink = attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    let readonly = attrs & FILE_ATTRIBUTE_READONLY != 0;
    let hidden = attrs & FILE_ATTRIBUTE_HIDDEN != 0;

    // Windows FILETIME is 100ns ticks since 1601-01-01; 0 means "not recorded".
    fn filetime_to_unix(ft: u64) -> Option<i64> {
        if ft == 0 {
            return None;
        }
        const TICKS_PER_SEC: u64 = 10_000_000;
        const EPOCH_DIFF_SECS: i64 = 11_644_473_600; // 1601-01-01 -> 1970-01-01
        Some((ft / TICKS_PER_SEC) as i64 - EPOCH_DIFF_SECS)
    }

    // Synthesised POSIX-ish mode for the permissions matrix: base perms by
    // type, write bits cleared when the read-only attribute is set.
    let base: u32 = if is_dir { 0o755 } else { 0o644 };
    let mode = if readonly { base & !0o222 } else { base };

    Some(StatInfo {
        size: md.file_size(),
        uid: 0,
        gid: 0,
        owner_name: std::env::var("USERNAME").unwrap_or_default(),
        group_name: String::new(),
        mode,
        is_symlink,
        is_dir,
        created_unix: filetime_to_unix(md.creation_time()),
        modified_unix: filetime_to_unix(md.last_write_time()).unwrap_or(0),
        accessed_unix: filetime_to_unix(md.last_access_time()),
        is_locked: readonly,
        is_invisible: hidden,
    })
}

#[cfg(not(any(unix, windows)))]
pub fn read_stat_info(_path: &Path) -> Option<StatInfo> {
    None
}

/// Write one filesystem timestamp without disturbing the other two.
///
/// Windows uses a `FILE_WRITE_ATTRIBUTES` handle rather than a normal writable
/// file handle: this works for directories and read-only files, and the null
/// pointers passed to `SetFileTime` preserve the untouched timestamps. The
/// value is read back after closing the handle so an ignored/provider-rounded
/// write is never reported to the UI as a successful exact edit.
#[cfg(windows)]
pub fn set_timestamp(path: &Path, kind: TimestampKind, unix: i64) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, SetFileTime, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, OPEN_EXISTING,
    };

    let timestamp = unix_to_filetime(unix)?;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let flags = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT;
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_WRITE_ATTRIBUTES.0,
            share,
            None,
            OPEN_EXISTING,
            flags,
            HANDLE::default(),
        )
    }
    .map_err(|error| format!("open for timestamp update: {error}"))?;

    let result = unsafe {
        match kind {
            TimestampKind::Created => SetFileTime(handle, Some(&timestamp), None, None),
            TimestampKind::Modified => SetFileTime(handle, None, None, Some(&timestamp)),
            TimestampKind::Accessed => SetFileTime(handle, None, Some(&timestamp), None),
        }
    }
    .map_err(|error| format!("SetFileTime: {error}"));
    let close_result = unsafe { CloseHandle(handle) }
        .map_err(|error| format!("CloseHandle after SetFileTime: {error}"));
    result.and(close_result)?;

    let info = read_stat_info(path)
        .ok_or_else(|| "read timestamp back after SetFileTime: path is unavailable".to_string())?;
    let actual = match kind {
        TimestampKind::Created => info.created_unix,
        TimestampKind::Modified => Some(info.modified_unix),
        TimestampKind::Accessed => info.accessed_unix,
    }
    .ok_or_else(|| {
        format!(
            "read timestamp back after SetFileTime: {:?} is unavailable",
            kind
        )
    })?;

    // FAT stores last-write time at two-second precision. Accept that normal
    // filesystem rounding but reject a provider that acknowledged the write
    // while retaining a materially different value.
    if actual.abs_diff(unix) > 2 {
        return Err(format!(
            "timestamp update was not retained (requested {unix}, read back {actual})"
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn unix_to_filetime(unix: i64) -> Result<windows::Win32::Foundation::FILETIME, String> {
    use windows::Win32::Foundation::FILETIME;

    const EPOCH_DIFF_SECS: i128 = 11_644_473_600;
    const TICKS_PER_SEC: i128 = 10_000_000;
    let ticks = (i128::from(unix) + EPOCH_DIFF_SECS)
        .checked_mul(TICKS_PER_SEC)
        .filter(|ticks| *ticks >= 0 && *ticks <= i128::from(u64::MAX))
        .ok_or_else(|| "date is outside the Windows filesystem range".to_string())?
        as u64;
    Ok(FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    })
}

/// Unix can portably update access and modification time. Creation/birth time
/// is intentionally left read-only: Linux generally cannot set it and macOS
/// needs a separate `setattrlist` implementation before we expose the control.
#[cfg(unix)]
pub fn set_timestamp(path: &Path, kind: TimestampKind, unix: i64) -> Result<(), String> {
    let value = filetime::FileTime::from_unix_time(unix, 0);
    let result = match kind {
        TimestampKind::Modified => filetime::set_file_mtime(path, value),
        TimestampKind::Accessed => filetime::set_file_atime(path, value),
        TimestampKind::Created => {
            return Err("creation-time editing is not supported on this platform".to_string());
        }
    };
    result.map_err(|error| format!("set file timestamp: {error}"))
}

#[cfg(not(any(unix, windows)))]
pub fn set_timestamp(_path: &Path, _kind: TimestampKind, _unix: i64) -> Result<(), String> {
    Err("timestamp editing is not supported on this platform".to_string())
}

/// Resolve a uid to an account name via `getpwuid_r`, falling back to the
/// numeric id. Thread-safe (the `_r` form takes a caller buffer).
#[cfg(unix)]
fn owner_name(uid: u32) -> String {
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        // `c_char` is i8 on x86/macOS but u8 on aarch64 Linux, never
        // spell the buffer element type out.
        let mut buf = [0 as libc::c_char; 1024];
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
        let mut buf = [0 as libc::c_char; 1024];
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

/// Windows "Locked" == the read-only file attribute; "Invisible" == the
/// hidden attribute. Both map onto the same `Attributes` toggles the Get Info
/// panel shows and the Properties dialog calls "Read-only" / "Hidden".
#[cfg(windows)]
pub fn set_locked(path: &Path, locked: bool) -> Result<(), String> {
    const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
    set_win_attr(path, FILE_ATTRIBUTE_READONLY, locked)
}

#[cfg(windows)]
pub fn set_invisible(path: &Path, invisible: bool) -> Result<(), String> {
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    set_win_attr(path, FILE_ATTRIBUTE_HIDDEN, invisible)
}

/// Read the current attribute mask, set or clear `attr`, write it back so the
/// other attributes survive. `windows`-crate `Get/SetFileAttributesW`.
#[cfg(windows)]
fn set_win_attr(path: &Path, attr: u32, on: bool) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetFileAttributesW, SetFileAttributesW, FILE_FLAGS_AND_ATTRIBUTES,
    };

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let current = GetFileAttributesW(PCWSTR(wide.as_ptr()));
        if current == u32::MAX {
            // INVALID_FILE_ATTRIBUTES
            return Err(format!(
                "GetFileAttributesW: {}",
                std::io::Error::last_os_error()
            ));
        }
        let updated = if on { current | attr } else { current & !attr };
        SetFileAttributesW(PCWSTR(wide.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(updated))
            .map_err(|e| format!("SetFileAttributesW: {e}"))?;
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn set_locked(_path: &Path, _locked: bool) -> Result<(), String> {
    Err("set_locked is macOS-only".into())
}

#[cfg(not(any(target_os = "macos", windows)))]
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

/// Filesystem type name (e.g. "apfs", "hfs", "exfat"), BSD device node
/// (e.g. "/dev/disk3s1s1"), and whether the volume is mounted read-only
/// (`MNT_RDONLY`, except the root mount: see below) for the volume
/// containing `path`. Strings `None` and read-only `false` off macOS or
/// on `statfs` failure.
#[cfg(target_os = "macos")]
pub fn volume_fs_info(path: &Path) -> (Option<String>, Option<String>, bool) {
    use std::os::unix::ffi::OsStrExt;

    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return (None, None, false);
    };
    // SAFETY: zeroed `statfs` is valid input; on non-zero return we read
    // nothing from it.
    let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut sfs) };
    if rc != 0 {
        return (None, None, false);
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
    // Boot-volume special case: the sealed system snapshot statfs's as
    // MNT_RDONLY on every modern macOS, but the "Macintosh HD" the user
    // sees is writable: their files live on the firmlinked Data
    // volume. A "read-only" badge on the boot volume is technically
    // true of the snapshot and nonsense as a user message, so the root
    // mount never reports it. Real read-only media (CDs, locked cards,
    // ro images) mount elsewhere and keep the flag.
    let read_only = sfs.f_flags & libc::MNT_RDONLY as u32 != 0
        && read_cstr(&sfs.f_mntonname).as_deref() != Some("/");
    (
        read_cstr(&sfs.f_fstypename),
        read_cstr(&sfs.f_mntfromname),
        read_only,
    )
}

#[cfg(not(target_os = "macos"))]
pub fn volume_fs_info(_path: &Path) -> (Option<String>, Option<String>, bool) {
    (None, None, false)
}

/// The local timezone's current offset from UTC in seconds (positive
/// east), for local-midnight math in filter date predicates
/// (`ferail_core::filter_expr::DateCtx`). DST-aware via `localtime_r`;
/// 0 (UTC) where unavailable.
pub fn local_tz_offset_secs() -> i64 {
    #[cfg(unix)]
    {
        let t = ferail_core::now_unix() as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: `localtime_r` fills the caller-provided `tm`; the
        // time_t is passed by pointer and not retained.
        let res = unsafe { libc::localtime_r(&t, &mut tm) };
        if !res.is_null() {
            return tm.tm_gmtoff as i64;
        }
    }
    0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LocalDateTime {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

impl LocalDateTime {
    fn parse(input: &str) -> Result<Self, String> {
        let mut fields = input.split_whitespace();
        let date = fields.next().ok_or_else(date_format_error)?;
        let clock = fields.next().ok_or_else(date_format_error)?;
        if fields.next().is_some() {
            return Err(date_format_error());
        }
        let mut date_fields = date.split('-');
        let mut clock_fields = clock.split(':');
        let parsed = Self {
            year: parse_component(date_fields.next())?,
            month: parse_component(date_fields.next())?,
            day: parse_component(date_fields.next())?,
            hour: parse_component(clock_fields.next())?,
            minute: parse_component(clock_fields.next())?,
            second: parse_component(clock_fields.next())?,
        };
        if date_fields.next().is_some() || clock_fields.next().is_some() || !parsed.is_valid() {
            return Err(date_format_error());
        }
        Ok(parsed)
    }

    fn is_valid(self) -> bool {
        let leap = self.year % 4 == 0 && (self.year % 100 != 0 || self.year % 400 == 0);
        let max_day = match self.month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return false,
        };
        (1601..=9999).contains(&self.year)
            && (1..=max_day).contains(&self.day)
            && self.hour <= 23
            && self.minute <= 59
            && self.second <= 59
    }
}

fn parse_component<T: std::str::FromStr>(value: Option<&str>) -> Result<T, String> {
    value
        .and_then(|value| value.parse().ok())
        .ok_or_else(date_format_error)
}

fn date_format_error() -> String {
    "enter a valid local date as YYYY-MM-DD HH:MM:SS".to_string()
}

/// Parse the stable editor format as a local wall-clock and convert it to a
/// Unix timestamp. OS timezone APIs do the conversion so historical DST rules
/// are respected; a round trip rejects normalized/nonexistent local times.
pub fn parse_local_datetime(input: &str) -> Result<i64, String> {
    let local = LocalDateTime::parse(input)?;

    #[cfg(unix)]
    {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = local.year - 1900;
        tm.tm_mon = local.month as i32 - 1;
        tm.tm_mday = local.day as i32;
        tm.tm_hour = local.hour as i32;
        tm.tm_min = local.minute as i32;
        tm.tm_sec = local.second as i32;
        tm.tm_isdst = -1;
        // SAFETY: mktime only mutates the caller-owned `tm` and consults the
        // process timezone. It also normalizes invalid/nonexistent times;
        // compare the result below so that normalization never surprises the
        // user.
        let unix = unsafe { libc::mktime(&mut tm) };
        let normalized = LocalDateTime {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u32,
            day: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            second: tm.tm_sec as u32,
        };
        if normalized != local {
            return Err("that local time does not exist in the current timezone".to_string());
        }
        return Ok(unix as i64);
    }

    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows::Win32::System::Time::{
            SystemTimeToFileTime, TzSpecificLocalTimeToSystemTimeEx,
        };

        let local_st = SYSTEMTIME {
            wYear: local.year as u16,
            wMonth: local.month as u16,
            wDay: local.day as u16,
            wHour: local.hour as u16,
            wMinute: local.minute as u16,
            wSecond: local.second as u16,
            ..Default::default()
        };
        let mut utc = SYSTEMTIME::default();
        unsafe { TzSpecificLocalTimeToSystemTimeEx(None, &local_st, &mut utc) }
            .map_err(|_| date_format_error())?;
        let mut filetime = FILETIME::default();
        unsafe { SystemTimeToFileTime(&utc, &mut filetime) }.map_err(|_| date_format_error())?;
        let ticks = u64::from(filetime.dwHighDateTime) << 32 | u64::from(filetime.dwLowDateTime);
        const TICKS_PER_SEC: u64 = 10_000_000;
        const EPOCH_DIFF_SECS: i64 = 11_644_473_600;
        let unix = (ticks / TICKS_PER_SEC) as i64 - EPOCH_DIFF_SECS;
        if local_datetime_parts(unix) != Some(local) {
            return Err("that local time does not exist in the current timezone".to_string());
        }
        return Ok(unix);
    }

    #[allow(unreachable_code)]
    Err("local date conversion is not supported on this platform".to_string())
}

/// Stable, locale-independent value used inside the timestamp editor.
pub fn format_editable_local_datetime(unix: i64) -> String {
    local_datetime_parts(unix)
        .map(|date| {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                date.year, date.month, date.day, date.hour, date.minute, date.second
            )
        })
        .unwrap_or_default()
}

#[cfg(unix)]
fn local_datetime_parts(unix: i64) -> Option<LocalDateTime> {
    let t = unix as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` fills the caller-owned `tm` and retains nothing.
    let result = unsafe { libc::localtime_r(&t, &mut tm) };
    (!result.is_null()).then_some(LocalDateTime {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
    })
}

#[cfg(windows)]
fn local_datetime_parts(unix: i64) -> Option<LocalDateTime> {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTimeEx};

    let filetime = unix_to_filetime(unix).ok()?;
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe { FileTimeToSystemTime(&filetime, &mut utc) }.ok()?;
    unsafe { SystemTimeToTzSpecificLocalTimeEx(None, &utc, &mut local) }.ok()?;
    Some(LocalDateTime {
        year: i32::from(local.wYear),
        month: u32::from(local.wMonth),
        day: u32::from(local.wDay),
        hour: u32::from(local.wHour),
        minute: u32::from(local.wMinute),
        second: u32::from(local.wSecond),
    })
}

#[cfg(not(any(unix, windows)))]
fn local_datetime_parts(_unix: i64) -> Option<LocalDateTime> {
    None
}

/// Format a unix timestamp as a Finder-style local date-time,
/// e.g. "9 Mar 2024 at 12:11". Both Windows and Unix use their timezone API,
/// including historical daylight-saving transitions.
pub fn format_local_datetime(unix: i64) -> String {
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let Some(date) = local_datetime_parts(unix) else {
        return unix.to_string();
    };
    let month = NAMES[(date.month.saturating_sub(1) as usize).min(11)];
    format!(
        "{} {} {} at {:02}:{:02}",
        date.day, month, date.year, date.hour, date.minute
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn reads_self_stat() {
        let dir = std::env::temp_dir().join(format!("ferail-statinfo-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("ferail-chmod-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("ferail-lock-{}", std::process::id()));
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

    #[cfg(any(unix, windows))]
    #[test]
    fn editable_local_datetime_round_trips() {
        let input = "2024-05-06 12:34:56";
        let unix = parse_local_datetime(input).expect("valid local timestamp");
        assert_eq!(format_editable_local_datetime(unix), input);
        assert!(parse_local_datetime("2024-02-30 12:00:00").is_err());
        assert!(parse_local_datetime("2024-05-06").is_err());
        assert!(parse_local_datetime("2024-05-06 25:00:00").is_err());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn writable_timestamps_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "ferail-timestamps-{}-{}",
            std::process::id(),
            ferail_core::now_unix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("probe.txt");
        std::fs::write(&file, b"timestamp").unwrap();
        let original_created = read_stat_info(&file).unwrap().created_unix;

        // Keep modified/accessed after birthtime. APFS legitimately clamps a
        // file's birthtime when callers backdate mtime before it, which made
        // this round-trip test mutate the very field it expected unchanged.
        let file_time_base = original_created.unwrap_or(1_700_000_000);
        let modified = file_time_base.saturating_add(10);
        let accessed = file_time_base.saturating_add(20);
        set_timestamp(&file, TimestampKind::Modified, modified).unwrap();
        set_timestamp(&file, TimestampKind::Accessed, accessed).unwrap();
        let info = read_stat_info(&file).unwrap();
        assert_eq!(info.modified_unix, modified);
        assert_eq!(info.accessed_unix, Some(accessed));
        assert_eq!(info.created_unix, original_created);

        #[cfg(windows)]
        {
            // FILE_WRITE_ATTRIBUTES must work even when normal writes do not.
            set_locked(&file, true).unwrap();
            let created = file_time_base.saturating_add(5);
            set_timestamp(&file, TimestampKind::Created, created).unwrap();
            let info = read_stat_info(&file).unwrap();
            assert_eq!(info.created_unix, Some(created));
            assert_eq!(info.modified_unix, modified);
            assert_eq!(info.accessed_unix, Some(accessed));
            assert!(info.is_locked);
            set_locked(&file, false).unwrap();
        }

        let directory_time_base = read_stat_info(&dir)
            .unwrap()
            .created_unix
            .unwrap_or(1_700_000_000);
        #[cfg(windows)]
        let directory_created = directory_time_base.saturating_add(5);
        let directory_modified = directory_time_base.saturating_add(10);
        let directory_accessed = directory_time_base.saturating_add(20);
        #[cfg(windows)]
        set_timestamp(&dir, TimestampKind::Created, directory_created).unwrap();
        set_timestamp(&dir, TimestampKind::Modified, directory_modified).unwrap();
        set_timestamp(&dir, TimestampKind::Accessed, directory_accessed).unwrap();
        let info = read_stat_info(&dir).unwrap();
        #[cfg(windows)]
        assert_eq!(info.created_unix, Some(directory_created));
        assert_eq!(info.modified_unix, directory_modified);
        assert_eq!(info.accessed_unix, Some(directory_accessed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn boot_volume_is_apfs() {
        let (fmt, dev, read_only) = volume_fs_info(Path::new("/"));
        assert!(fmt.is_some(), "boot volume has a format");
        assert!(
            dev.as_deref().unwrap_or("").starts_with("/dev/"),
            "device node looks like /dev/...: {dev:?}"
        );
        // The sealed snapshot statfs's as MNT_RDONLY, but the boot
        // volume is deliberately exempt: the user-visible Macintosh HD
        // is writable via the firmlinked Data volume, so a read-only
        // badge there would be nonsense.
        assert!(!read_only, "the boot volume never reports read-only");
    }
}
