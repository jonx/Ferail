//! Volume / drive enumeration.
//!
//! macOS reads the kernel mount table and resolves NSURL keys for each
//! browsable mount under a deadline;
//! Windows enumerates drive letters via `GetLogicalDrives` and
//! classifies / measures each one via `GetDriveTypeW` +
//! `GetVolumeInformationW` + `GetDiskFreeSpaceExW`.
//!
//! v1 is pure synchronous Win32: fine because drive enumeration is
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
    // crate 0.58 doesn't re-export these constants: they've been
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
        // Skip A: / B: floppy slots and unknown: they appear in the
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
        // timeout (tens of seconds): the bare letter is an honest
        // label, and the sidebar refresh picks up a nicer one when the
        // share is actually reachable via the capacity-free path.
        // Read-only detection rides along for free: 0x00080000 is
        // FILE_READ_ONLY_VOLUME (stable since XP; the `windows` crate
        // 0.58 doesn't re-export it). Set for CD/DVD media, locked SD
        // cards, and NTFS/ReFS volumes mounted read-only.
        const FILE_READ_ONLY_VOLUME: u32 = 0x0008_0000;
        let mut read_only = false;
        let name = if kind == DRIVE_REMOTE {
            format!("{letter}:")
        } else {
            let mut name_buf = [0u16; 261];
            let mut fs_flags: u32 = 0;
            unsafe {
                match GetVolumeInformationW(
                    root_pcwstr,
                    Some(&mut name_buf),
                    None,
                    None,
                    Some(&mut fs_flags),
                    None,
                ) {
                    Ok(()) => {
                        read_only = fs_flags & FILE_READ_ONLY_VOLUME != 0;
                        let len = name_buf
                            .iter()
                            .position(|&c| c == 0)
                            .unwrap_or(name_buf.len());
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

        // Capacity. Only ask for it on local drives: network shares
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

        // Physical-device probe (skipped for network drives, no local
        // device to ask). Two things GetDriveTypeW can't tell us: which
        // physical disk backs the volume (the eject-all grouping key),
        // and whether a DRIVE_FIXED disk actually hangs off USB: external
        // HDDs/SSDs report as fixed, but are ejectable like Finder's.
        let (device_id, usb_bus) = if kind == DRIVE_REMOTE {
            (None, false)
        } else {
            probe_volume_device(letter)
        };
        let is_removable = is_removable || usb_bus;

        out.push(VolumeInfo {
            path: format!("{letter}:\\").into(),
            name,
            total_bytes,
            available_bytes,
            is_local,
            is_removable,
            format: None,
            read_only,
            bsd_device: None,
            device_id,
            responding: true,
        });
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Physical-device probe for the volume behind drive `letter`: the
/// physical disk number(s) backing it (`Some("disk3")`: the eject-all
/// grouping key) and whether the disk hangs off the USB bus. The latter
/// matters because `GetDriveTypeW` reports external USB HDDs/SSDs as
/// `DRIVE_FIXED`, hiding that they're ejectable.
///
/// Opens the volume with **zero** access rights: metadata-only queries,
/// no media I/O, no admin required. Best-effort: `(None, false)` on any
/// failure (CD-ROM volumes and RAM disks have no disk extents).
#[cfg(windows)]
fn probe_volume_device(letter: char) -> (Option<String>, bool) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        BusTypeUsb, CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::{
        PropertyStandardQuery, StorageDeviceProperty, IOCTL_STORAGE_QUERY_PROPERTY,
        STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY, VOLUME_DISK_EXTENTS,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    let device = format!(r"\\.\{letter}:");
    let wide: Vec<u16> = device.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let Ok(handle) = CreateFileW(
            PCWSTR(wide.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        ) else {
            return (None, false);
        };

        // Disk extents → physical disk number(s). One extent for plain
        // volumes; a spanned volume lists several, which we join
        // ("disk1+3") so it only ever groups with an identical span.
        // u64 buffer for the 8-byte alignment VOLUME_DISK_EXTENTS wants.
        let mut extents_buf = [0u64; 64];
        let mut returned = 0u32;
        let device_id = DeviceIoControl(
            handle,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            None,
            0,
            Some(extents_buf.as_mut_ptr() as *mut core::ffi::c_void),
            std::mem::size_of_val(&extents_buf) as u32,
            Some(&mut returned),
            None,
        )
        .ok()
        .and_then(|()| {
            let info = &*(extents_buf.as_ptr() as *const VOLUME_DISK_EXTENTS);
            // Extents is declared `[DISK_EXTENT; 1]`; further extents
            // follow contiguously in the buffer. Cap at what the buffer
            // can actually hold.
            let n = (info.NumberOfDiskExtents as usize).min(20);
            let first = info.Extents.as_ptr();
            let mut disks: Vec<u32> = (0..n).map(|i| (*first.add(i)).DiskNumber).collect();
            disks.sort_unstable();
            disks.dedup();
            if disks.is_empty() {
                None
            } else {
                let nums: Vec<String> = disks.iter().map(u32::to_string).collect();
                Some(format!("disk{}", nums.join("+")))
            }
        });

        // Bus type: is the backing device on USB? (SD/MMC readers report
        // DRIVE_REMOVABLE already; USB enclosures are the blind spot.)
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut desc_buf = [0u64; 128];
        let usb_bus = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const core::ffi::c_void),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(desc_buf.as_mut_ptr() as *mut core::ffi::c_void),
            std::mem::size_of_val(&desc_buf) as u32,
            Some(&mut returned),
            None,
        )
        .map(|()| {
            let desc = &*(desc_buf.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR);
            desc.BusType == BusTypeUsb
        })
        .unwrap_or(false);

        let _ = CloseHandle(handle);
        (device_id, usb_bus)
    }
}

/// How long one volume may take to answer its metadata lookup before it is
/// listed as not responding. Healthy volumes, network shares included, answer
/// from cached keys in milliseconds.
#[cfg(target_os = "macos")]
const VOLUME_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// macOS volumes, the way Finder's sidebar lists them: the mounted,
/// browsable filesystems, one row per filesystem.
///
/// The mount table is read with `getfsstat(MNT_NOWAIT)`, which answers from
/// the kernel's cached copy without asking any filesystem, so a volume that
/// has stopped answering cannot stall the listing. Each volume is then
/// resolved on its own thread under [`VOLUME_PROBE_TIMEOUT`]; one that does
/// not answer in time keeps the facts the mount table already gave and is
/// marked `responding: false`, and never hides the volumes after it.
#[cfg(target_os = "macos")]
pub fn list_volumes() -> Vec<VolumeInfo> {
    use crate::deadline;

    let deadline_at = std::time::Instant::now() + VOLUME_PROBE_TIMEOUT;
    let probes: Vec<(MountEntry, deadline::Probe<Option<VolumeInfo>>)> =
        browsable_mounts(mounted_filesystems())
            .into_iter()
            .map(|mount| {
                let path = mount.path.clone();
                let probe = deadline::start(("volume-info", path.clone()), move || {
                    crate::volume_info_for_path(&path)
                });
                (mount, probe)
            })
            .collect();
    let mut out: Vec<VolumeInfo> = probes
        .into_iter()
        .map(|(mount, probe)| match probe.wait_until(deadline_at) {
            Some(Some(info)) => info,
            Some(None) => mount.into_info(true),
            None => mount.into_info(false),
        })
        .collect();
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// One row of the kernel mount table, reduced to what the volume list needs.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MountEntry {
    pub path: std::path::PathBuf,
    pub fs_type: String,
    pub source: String,
    pub fsid: [i32; 2],
    pub browsable: bool,
    pub local: bool,
    pub read_only: bool,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl MountEntry {
    /// The volume as the mount table alone describes it, for when the
    /// metadata lookup failed or never answered.
    fn into_info(self, responding: bool) -> VolumeInfo {
        let name = self
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| self.path.display().to_string());
        let is_root = self.path == std::path::Path::new("/");
        let device_id = if self.local {
            whole_disk_bsd(&self.source)
        } else {
            None
        };
        VolumeInfo {
            name,
            total_bytes: None,
            available_bytes: None,
            is_local: self.local,
            // Offer Eject on anything but the boot volume: unmounting is the
            // way out of a volume that has stopped answering.
            is_removable: !is_root,
            format: Some(self.fs_type),
            read_only: self.read_only && !is_root,
            bsd_device: self.local.then_some(self.source),
            device_id,
            responding,
            path: self.path,
        }
    }
}

/// The mounts a user browses: drop what Finder hides (`MNT_DONTBROWSE`,
/// which covers the Data half of an APFS volume group, Preboot, Recovery,
/// VM and Update), the automounter's trigger points, and a second mount of a
/// filesystem already listed.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn browsable_mounts(mounts: Vec<MountEntry>) -> Vec<MountEntry> {
    let mut seen: std::collections::HashSet<[i32; 2]> = std::collections::HashSet::new();
    mounts
        .into_iter()
        .filter(|m| m.browsable && m.fs_type != "autofs" && m.fs_type != "devfs")
        .filter(|m| m.fsid == [0, 0] || seen.insert(m.fsid))
        .collect()
}

/// The kernel mount table, from its cached copy (`MNT_NOWAIT`): no
/// filesystem is asked anything, so a dead mount costs nothing here.
#[cfg(target_os = "macos")]
pub(crate) fn mounted_filesystems() -> Vec<MountEntry> {
    use std::os::unix::ffi::OsStrExt;

    // Sized by a first call, with headroom for a mount that lands between the
    // two calls. `getfsstat` rather than `getmntinfo`: the latter returns a
    // shared static buffer and is not thread-safe.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count <= 0 {
        return Vec::new();
    }
    let capacity = count as usize + 8;
    let mut buf: Vec<libc::statfs> = Vec::with_capacity(capacity);
    let bytes = (capacity * std::mem::size_of::<libc::statfs>()) as libc::c_int;
    // SAFETY: the buffer holds `capacity` statfs records and the kernel
    // writes at most `bytes` of them, returning how many it wrote.
    let got = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    if got <= 0 {
        return Vec::new();
    }
    unsafe { buf.set_len((got as usize).min(capacity)) };

    let text = |arr: &[libc::c_char]| -> String {
        // SAFETY: the kernel NUL-terminates these fixed-size fields.
        unsafe { std::ffi::CStr::from_ptr(arr.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    };
    buf.iter()
        .map(|sfs| {
            let on = unsafe { std::ffi::CStr::from_ptr(sfs.f_mntonname.as_ptr()) };
            let flags = sfs.f_flags;
            // SAFETY: fsid_t is two i32s; libc keeps the field private.
            let fsid: [i32; 2] = unsafe { std::mem::transmute(sfs.f_fsid) };
            MountEntry {
                path: std::path::PathBuf::from(std::ffi::OsStr::from_bytes(on.to_bytes())),
                fs_type: text(&sfs.f_fstypename),
                source: text(&sfs.f_mntfromname),
                fsid,
                browsable: flags & libc::MNT_DONTBROWSE as u32 == 0,
                local: flags & libc::MNT_LOCAL as u32 != 0,
                read_only: flags & libc::MNT_RDONLY as u32 != 0,
            }
        })
        .collect()
}

/// Linux volumes from `/proc/self/mountinfo`: the root filesystem, `/home`,
/// and removable / user mounts (`/media`, `/run/media`, `/mnt`) that sit on a
/// real block device, skipping the pile of pseudo/virtual filesystems. Sizes
/// come from `statvfs`. Mirrors the macOS `/Volumes` enumeration for the
/// sidebar's Volumes section.
#[cfg(target_os = "linux")]
pub fn list_volumes() -> Vec<VolumeInfo> {
    use std::collections::HashSet;
    use std::io::{BufRead, BufReader};

    // Virtual / pseudo filesystems that are never user "volumes".
    const PSEUDO_FS: &[&str] = &[
        "proc",
        "sysfs",
        "tmpfs",
        "devtmpfs",
        "devpts",
        "cgroup",
        "cgroup2",
        "mqueue",
        "hugetlbfs",
        "debugfs",
        "tracefs",
        "securityfs",
        "pstore",
        "bpf",
        "configfs",
        "fusectl",
        "binfmt_misc",
        "autofs",
        "ramfs",
        "rpc_pipefs",
        "nsfs",
        "overlay",
        "squashfs",
        "efivarfs",
        "selinuxfs",
        "fuse.gvfsd-fuse",
        "fuse.portal",
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

        let (total, available, read_only) = statvfs_bytes(&mount_point);
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
            read_only,
            bsd_device: Some(source.to_string()),
            device_id: linux_device_group(source),
            responding: true,
        });
    }

    // Root first, then alphabetical.
    out.sort_by(|a, b| {
        let ar = a.path == std::path::Path::new("/");
        let br = b.path == std::path::Path::new("/");
        br.cmp(&ar)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
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

/// Total + available bytes and the read-only mount flag (`ST_RDONLY`)
/// for the filesystem at `path` via `statvfs`.
#[cfg(target_os = "linux")]
fn statvfs_bytes(path: &str) -> (Option<u64>, Option<u64>, bool) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(std::ffi::OsStr::new(path).as_bytes()) else {
        return (None, None, false);
    };
    // SAFETY: zeroed statvfs is a valid initial state; on non-zero return we
    // read nothing from it.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut st) } != 0 {
        return (None, None, false);
    }
    let frsize = st.f_frsize as u64;
    let total = (st.f_blocks as u64).checked_mul(frsize);
    let avail = (st.f_bavail as u64).checked_mul(frsize);
    (total, avail, st.f_flag & libc::ST_RDONLY != 0)
}

/// AROS volumes for the sidebar's Volumes section. AROS has no `/proc/mounts`
/// or `/Volumes` to enumerate, and iterating the DOS device list needs
/// dos.library FFI we don't bind yet, so probe the volumes a stock hosted
/// AROS mounts by default and keep the ones that resolve. `exists()` stats
/// (never `open()`s), so this can't trip the posixc open() fault. Colon-
/// terminated AROS volume names double as their root path.
#[cfg(target_os = "aros")]
pub fn list_volumes() -> Vec<VolumeInfo> {
    // Only volumes/assigns that resolve without a "Please insert volume"
    // requester: SYS:/RAM: are always mounted; MacRO:/MacRW: are assigns that
    // fail cleanly when absent. A stock named volume like Work: would pop the
    // AmigaOS insert-media requester when unmounted, so it is intentionally out.
    const CANDIDATES: &[(&str, &str)] = &[
        ("System", "SYS:"),
        ("Ram Disk", "RAM:"),
        ("Mac (read-only)", "MacRO:"),
        ("Mac (read/write)", "MacRW:"),
    ];
    CANDIDATES
        .iter()
        .filter(|(_, path)| std::path::Path::new(path).exists())
        .map(|&(name, path)| VolumeInfo {
            name: name.to_string(),
            path: std::path::PathBuf::from(path),
            total_bytes: None,
            available_bytes: None,
            is_local: true,
            is_removable: false,
            format: None,
            // MacRO: is the host-folder assign mounted without write
            // access: the name already says so; the flag makes the
            // status bar say it too.
            read_only: path == "MacRO:",
            bsd_device: None,
            device_id: None,
            responding: true,
        })
        .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "aros", windows)))]
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

/// Whole-disk BSD name from a device node: "/dev/disk3s1s1" → "disk3".
/// `None` when the name isn't `diskN…`-shaped (network sources, synthetic
/// mounts). APFS note: volumes of one container share a *synthesized*
/// whole disk, so they group together, exactly what the eject-all offer
/// wants. A mixed-scheme disk (APFS container next to a FAT partition)
/// groups as two devices; acceptable miss, Finder-parity for the common
/// case. Compiled on all hosts so the unit test runs everywhere (hence
/// the allow, only macOS builds reach it outside tests).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn whole_disk_bsd(dev: &str) -> Option<String> {
    let name = dev.strip_prefix("/dev/").unwrap_or(dev);
    let rest = name.strip_prefix("disk")?;
    let digits: &str = &rest[..rest.chars().take_while(|c| c.is_ascii_digit()).count()];
    if digits.is_empty() {
        return None;
    }
    Some(format!("disk{digits}"))
}

/// Parent disk name for a Linux partition device name, by string shape
/// alone: "sdb1" → "sdb", "nvme0n1p2" → "nvme0n1", "mmcblk0p1" →
/// "mmcblk0". Returns `None` when the name doesn't look like a partition
/// (whole disks, `dm-0`, `loop0`: safer to not group than to mis-group).
/// Fallback for when sysfs isn't available; compiled on all hosts so the
/// unit test runs everywhere (hence the allow, only Linux builds reach
/// it outside tests).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn linux_parent_disk_name(name: &str) -> Option<String> {
    // nvme0n1p2 / mmcblk0p1 style: "<disk ending in a digit>p<digits>".
    if let Some(pos) = name.rfind('p') {
        let (disk, part) = (&name[..pos], &name[pos + 1..]);
        if !part.is_empty()
            && part.chars().all(|c| c.is_ascii_digit())
            && disk.ends_with(|c: char| c.is_ascii_digit())
        {
            return Some(disk.to_string());
        }
    }
    // sdb1 / vda2 / xvdf3 style: letters then digits, for known prefixes
    // only ("loop0" and friends are whole devices, not partitions).
    for prefix in ["sd", "hd", "vd", "xvd"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            let letters = rest.chars().take_while(|c| c.is_ascii_alphabetic()).count();
            let digits = &rest[letters..];
            if letters > 0 && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                return Some(format!("{prefix}{}", &rest[..letters]));
            }
        }
    }
    None
}

/// Whole-device grouping key for a Linux mount source like "/dev/sdb1".
/// Prefers sysfs (authoritative: `/sys/class/block/<name>` resolves into
/// the parent disk's directory for partitions), falling back to the
/// string-shape parse. Whole-disk sources (no partition table) group
/// under their own name.
#[cfg(target_os = "linux")]
fn linux_device_group(source: &str) -> Option<String> {
    let name = source.strip_prefix("/dev/")?;
    if name.contains('/') {
        return None; // /dev/mapper/… etc.: don't guess.
    }
    let sys = std::path::Path::new("/sys/class/block").join(name);
    if sys.join("partition").exists() {
        // Partition: the sysfs node lives at …/block/<disk>/<part>.
        if let Ok(target) = std::fs::read_link(&sys) {
            if let Some(disk) = target
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
            {
                if disk != "block" {
                    return Some(disk.to_string());
                }
            }
        }
        return linux_parent_disk_name(name);
    }
    if sys.exists() {
        return Some(name.to_string()); // Whole disk mounted directly.
    }
    linux_parent_disk_name(name).or_else(|| Some(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(path: &str, fsid: [i32; 2], browsable: bool) -> MountEntry {
        MountEntry {
            path: path.into(),
            fs_type: "apfs".into(),
            source: "/dev/disk9s1".into(),
            fsid,
            browsable,
            local: true,
            read_only: false,
        }
    }

    #[test]
    fn browsable_mounts_hide_what_finder_hides_and_list_each_filesystem_once() {
        let mut automount = mount("/System/Volumes/Data/home", [9, 9], true);
        automount.fs_type = "autofs".into();
        let kept = browsable_mounts(vec![
            mount("/", [1, 1], true),
            mount("/System/Volumes/Data", [2, 2], false),
            mount("/Volumes/Alpha", [3, 3], true),
            mount("/Volumes/Alpha - Data", [4, 4], false),
            mount("/private/tmp/second-mount-of-alpha", [3, 3], true),
            automount,
        ]);
        let paths: Vec<_> = kept.iter().map(|m| m.path.to_str().unwrap()).collect();
        assert_eq!(paths, ["/", "/Volumes/Alpha"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_live_list_has_the_boot_volume_once_and_no_hidden_volumes() {
        let volumes = list_volumes();
        let roots = volumes.iter().filter(|v| v.path == std::path::Path::new("/")).count();
        assert_eq!(roots, 1, "{volumes:?}");
        assert!(
            volumes.iter().all(|v| v.path != std::path::Path::new("/System/Volumes/Data")),
            "{volumes:?}"
        );
        assert!(volumes.iter().all(|v| v.responding), "{volumes:?}");
    }

    #[test]
    fn an_unanswered_volume_keeps_its_mount_table_facts() {
        let mut m = mount("/Volumes/Stalled", [5, 5], true);
        m.fs_type = "macfuse".into();
        let info = m.into_info(false);
        assert_eq!(info.name, "Stalled");
        assert!(!info.responding);
        assert_eq!(info.format.as_deref(), Some("macfuse"));
        assert_eq!(info.device_id.as_deref(), Some("disk9"));
        assert!(info.total_bytes.is_none());
    }

    #[test]
    fn whole_disk_bsd_parses_slices_and_rejects_non_disks() {
        assert_eq!(whole_disk_bsd("/dev/disk3s1s1").as_deref(), Some("disk3"));
        assert_eq!(whole_disk_bsd("/dev/disk12s2").as_deref(), Some("disk12"));
        assert_eq!(whole_disk_bsd("disk4").as_deref(), Some("disk4"));
        assert_eq!(whole_disk_bsd("//host/share"), None);
        assert_eq!(whole_disk_bsd("/dev/diskXs1"), None);
        assert_eq!(whole_disk_bsd("map auto_home"), None);
    }

    #[test]
    fn linux_parent_disk_parses_partitions_only() {
        assert_eq!(linux_parent_disk_name("sdb1").as_deref(), Some("sdb"));
        assert_eq!(linux_parent_disk_name("xvdf3").as_deref(), Some("xvdf"));
        assert_eq!(
            linux_parent_disk_name("nvme0n1p2").as_deref(),
            Some("nvme0n1")
        );
        assert_eq!(
            linux_parent_disk_name("mmcblk0p1").as_deref(),
            Some("mmcblk0")
        );
        // Whole devices and unknown shapes stay ungrouped.
        assert_eq!(linux_parent_disk_name("sdb"), None);
        assert_eq!(linux_parent_disk_name("nvme0n1"), None);
        assert_eq!(linux_parent_disk_name("loop0"), None);
        assert_eq!(linux_parent_disk_name("dm-0"), None);
    }
}
