use std::ffi::OsStr;
use std::fmt;
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ferail_ntfs::{
    parse_boot_sector, ByteReader, FileIdentity, NtfsError, NtfsGeometry, Result as NtfsResult,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FileIdInfo, GetDriveTypeW, GetFileInformationByHandleEx, GetVolumeInformationW,
    GetVolumeNameForVolumeMountPointW, GetVolumePathNameW, ReadFile, SetFilePointerEx, FILE_BEGIN,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_SEQUENTIAL_SCAN, FILE_ID_INFO, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::{
    FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_QUERY_USN_JOURNAL, NTFS_VOLUME_DATA_BUFFER,
    USN_JOURNAL_DATA_V0,
};
use windows::Win32::System::WindowsProgramming::DRIVE_FIXED;
use windows::Win32::System::IO::DeviceIoControl;

const MAX_WIDE_PATH: usize = 32_768;
const DEFAULT_CACHE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FastNtfsProbe {
    pub requested_root: PathBuf,
    pub volume_mount: PathBuf,
    /// Canonical `\\?\Volume{GUID}\` form. This is private protocol data and
    /// must not be written to normal logs.
    pub volume_guid: Vec<u16>,
    pub volume_serial: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawNtfsGeometry {
    pub parsed: NtfsGeometry,
    pub mft_valid_bytes: u64,
    pub total_clusters: u64,
    pub free_clusters: u64,
}

#[derive(Debug)]
pub enum RawVolumeError {
    Unsupported(&'static str),
    InvalidPath(&'static str),
    Win32(&'static str, windows::core::Error),
    Geometry(&'static str),
    Parser(NtfsError),
    Poisoned,
}

impl fmt::Display for RawVolumeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(context) => write!(f, "Fast NTFS unsupported: {context}"),
            Self::InvalidPath(context) => write!(f, "invalid Fast NTFS path: {context}"),
            Self::Win32(context, error) => write!(f, "{context}: {error}"),
            Self::Geometry(context) => write!(f, "invalid NTFS geometry: {context}"),
            Self::Parser(error) => error.fmt(f),
            Self::Poisoned => f.write_str("raw-volume read lock poisoned"),
        }
    }
}

impl std::error::Error for RawVolumeError {}

impl From<NtfsError> for RawVolumeError {
    fn from(value: NtfsError) -> Self {
        Self::Parser(value)
    }
}

pub type Result<T> = std::result::Result<T, RawVolumeError>;

pub fn probe_fast_ntfs(path: &Path) -> Result<FastNtfsProbe> {
    if path.as_os_str().is_empty() {
        return Err(RawVolumeError::InvalidPath("empty path"));
    }
    let requested_root = path.to_path_buf();
    let input = wide_nul(path.as_os_str())?;
    let mut mount = vec![0u16; MAX_WIDE_PATH];
    unsafe { GetVolumePathNameW(PCWSTR(input.as_ptr()), &mut mount) }
        .map_err(|error| RawVolumeError::Win32("GetVolumePathNameW", error))?;
    truncate_at_nul(&mut mount);
    if mount.is_empty() {
        return Err(RawVolumeError::InvalidPath("empty volume mount"));
    }

    let mut mount_nul = mount.clone();
    mount_nul.push(0);
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(mount_nul.as_ptr())) };
    if drive_type != DRIVE_FIXED {
        return Err(RawVolumeError::Unsupported(
            "volume is not a local fixed drive",
        ));
    }

    let mut filesystem = vec![0u16; 32];
    let mut serial = 0u32;
    unsafe {
        GetVolumeInformationW(
            PCWSTR(mount_nul.as_ptr()),
            None,
            Some(&mut serial),
            None,
            None,
            Some(&mut filesystem),
        )
    }
    .map_err(|error| RawVolumeError::Win32("GetVolumeInformationW", error))?;
    truncate_at_nul(&mut filesystem);
    if !filesystem.eq_ignore_ascii_case(&"NTFS".encode_utf16().collect::<Vec<_>>()) {
        return Err(RawVolumeError::Unsupported("filesystem is not NTFS"));
    }

    let mut volume_guid = vec![0u16; MAX_WIDE_PATH];
    unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(mount_nul.as_ptr()), &mut volume_guid) }
        .map_err(|error| RawVolumeError::Win32("GetVolumeNameForVolumeMountPointW", error))?;
    truncate_at_nul(&mut volume_guid);
    if volume_guid.last() != Some(&(b'\\' as u16)) {
        return Err(RawVolumeError::Geometry(
            "volume GUID lacks trailing separator",
        ));
    }

    Ok(FastNtfsProbe {
        requested_root,
        volume_mount: PathBuf::from(String::from_utf16_lossy(&mount)),
        volume_guid,
        volume_serial: serial,
    })
}

pub struct RawVolumeReader {
    handle: OwnedHandle,
    volume_bytes: u64,
    sector_size: usize,
    cache: Mutex<ReadCache>,
    geometry: RawNtfsGeometry,
}

impl RawVolumeReader {
    /// Opens the exact probed volume read-only. Call this only inside the
    /// explicitly elevated helper; the GUI must never call it speculatively.
    pub fn open(probe: &FastNtfsProbe) -> Result<Self> {
        let mut device = probe.volume_guid.clone();
        if device.pop() != Some(b'\\' as u16) {
            return Err(RawVolumeError::Geometry("invalid volume GUID"));
        }
        device.push(0);
        let share = FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0);
        let handle = unsafe {
            CreateFileW(
                PCWSTR(device.as_ptr()),
                GENERIC_READ.0,
                share,
                None,
                OPEN_EXISTING,
                FILE_FLAG_SEQUENTIAL_SCAN,
                None,
            )
        }
        .map_err(|error| RawVolumeError::Win32("open raw volume read-only", error))?;
        let handle = OwnedHandle(handle);

        let ioctl = query_ntfs_geometry(handle.0)?;
        let sector_size = usize::try_from(ioctl.BytesPerSector)
            .map_err(|_| RawVolumeError::Geometry("sector size does not fit usize"))?;
        let volume_bytes = positive_i64(ioctl.NumberSectors, "negative sector count")?
            .checked_mul(u64::from(ioctl.BytesPerSector))
            .ok_or(RawVolumeError::Geometry("volume byte length overflow"))?;
        let cache_bytes = DEFAULT_CACHE_BYTES
            .max(sector_size)
            .checked_div(sector_size)
            .and_then(|sectors| sectors.checked_mul(sector_size))
            .ok_or(RawVolumeError::Geometry("cache alignment overflow"))?;

        let mut reader = Self {
            handle,
            volume_bytes,
            sector_size,
            cache: Mutex::new(ReadCache {
                start: 0,
                valid: 0,
                bytes: vec![0u8; cache_bytes],
            }),
            geometry: RawNtfsGeometry {
                parsed: NtfsGeometry {
                    bytes_per_sector: 0,
                    sectors_per_cluster: 0,
                    bytes_per_cluster: 0,
                    bytes_per_file_record: 0,
                    total_sectors: 0,
                    mft_start_lcn: 0,
                    mft_mirror_lcn: 0,
                    volume_serial: 0,
                },
                mft_valid_bytes: 0,
                total_clusters: 0,
                free_clusters: 0,
            },
        };
        let parsed = parse_boot_sector(&reader)?;
        cross_check_geometry(&parsed, &ioctl, probe.volume_serial)?;
        reader.geometry = RawNtfsGeometry {
            parsed,
            mft_valid_bytes: positive_i64(ioctl.MftValidDataLength, "negative MFT length")?,
            total_clusters: positive_i64(ioctl.TotalClusters, "negative cluster count")?,
            free_clusters: positive_i64(ioctl.FreeClusters, "negative free cluster count")?,
        };
        Ok(reader)
    }

    pub const fn geometry(&self) -> RawNtfsGeometry {
        self.geometry
    }

    /// Returns journal identity/cursor when the volume has a USN journal.
    /// Absence is a supported state and is represented as `None`.
    pub fn journal_position(&self) -> Option<(u64, i64)> {
        let mut data = USN_JOURNAL_DATA_V0::default();
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                self.handle.0,
                FSCTL_QUERY_USN_JOURNAL,
                None,
                0,
                Some((&mut data as *mut USN_JOURNAL_DATA_V0).cast()),
                std::mem::size_of::<USN_JOURNAL_DATA_V0>() as u32,
                Some(&mut returned),
                None,
            )
        }
        .ok()?;
        (returned >= std::mem::size_of::<USN_JOURNAL_DATA_V0>() as u32)
            .then_some((data.UsnJournalID, data.NextUsn))
    }
}

/// Opens a directory/file without reading its contents and returns the stable
/// volume/file identity used in the authenticated Start frame, plus the NTFS
/// record number encoded by the filesystem in the low 48 bits.
pub fn file_identity(path: &Path) -> Result<(FileIdentity, u64)> {
    let wide = wide_nul(path.as_os_str())?;
    let share = FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0);
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            share,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map_err(|error| RawVolumeError::Win32("open root identity", error))?;
    let handle = OwnedHandle(handle);
    let mut info = FILE_ID_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            handle.0,
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .map_err(|error| RawVolumeError::Win32("read root identity", error))?;
    let file_id = info.FileId.Identifier;
    let reference = u64::from_le_bytes(file_id[..8].try_into().expect("FILE_ID prefix"));
    Ok((
        FileIdentity {
            volume_serial: info.VolumeSerialNumber,
            file_id,
        },
        reference & 0x0000_ffff_ffff_ffff,
    ))
}

impl ByteReader for RawVolumeReader {
    fn len(&self) -> u64 {
        self.volume_bytes
    }

    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> NtfsResult<()> {
        let end = offset
            .checked_add(destination.len() as u64)
            .ok_or_else(|| NtfsError::source(offset, "raw read range overflow"))?;
        if end > self.volume_bytes {
            return Err(NtfsError::source(offset, "raw read outside volume"));
        }
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| NtfsError::source(offset, "raw read cache poisoned"))?;
        let mut source_offset = offset;
        let mut written = 0usize;
        while written < destination.len() {
            if source_offset < cache.start
                || source_offset >= cache.start.saturating_add(cache.valid as u64)
            {
                fill_cache(
                    self.handle.0,
                    self.volume_bytes,
                    self.sector_size,
                    source_offset,
                    &mut cache,
                )?;
            }
            let within = usize::try_from(source_offset - cache.start)
                .map_err(|_| NtfsError::source(source_offset, "cache offset overflow"))?;
            let available = cache.valid.saturating_sub(within);
            if available == 0 {
                return Err(NtfsError::source(source_offset, "zero-byte raw read"));
            }
            let count = available.min(destination.len() - written);
            destination[written..written + count]
                .copy_from_slice(&cache.bytes[within..within + count]);
            written += count;
            source_offset = source_offset
                .checked_add(count as u64)
                .ok_or_else(|| NtfsError::source(source_offset, "raw read cursor overflow"))?;
        }
        Ok(())
    }
}

struct OwnedHandle(HANDLE);

// SAFETY: the kernel handle is process-owned, read-only and all mutable file-
// pointer access is serialized by RawVolumeReader::cache.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct ReadCache {
    start: u64,
    valid: usize,
    bytes: Vec<u8>,
}

fn fill_cache(
    handle: HANDLE,
    volume_bytes: u64,
    sector_size: usize,
    requested: u64,
    cache: &mut ReadCache,
) -> NtfsResult<()> {
    let sector = sector_size as u64;
    let start = requested / sector * sector;
    let remaining = volume_bytes
        .checked_sub(start)
        .ok_or_else(|| NtfsError::source(requested, "cache start outside volume"))?;
    let wanted = usize::try_from(remaining.min(cache.bytes.len() as u64))
        .map_err(|_| NtfsError::source(start, "cache read length overflow"))?;
    if wanted == 0 {
        return Err(NtfsError::source(start, "empty cache fill"));
    }
    let seek = i64::try_from(start)
        .map_err(|_| NtfsError::source(start, "volume offset exceeds signed seek"))?;
    unsafe { SetFilePointerEx(handle, seek, None, FILE_BEGIN) }
        .map_err(|_| NtfsError::source(start, "raw volume seek failed"))?;

    let mut read_total = 0usize;
    while read_total < wanted {
        let mut read = 0u32;
        unsafe {
            ReadFile(
                handle,
                Some(&mut cache.bytes[read_total..wanted]),
                Some(&mut read),
                None,
            )
        }
        .map_err(|_| NtfsError::source(start + read_total as u64, "raw volume read failed"))?;
        if read == 0 {
            return Err(NtfsError::source(
                start + read_total as u64,
                "unexpected raw volume EOF",
            ));
        }
        read_total = read_total
            .checked_add(read as usize)
            .ok_or_else(|| NtfsError::source(start, "raw read count overflow"))?;
    }
    cache.start = start;
    cache.valid = read_total;
    Ok(())
}

fn query_ntfs_geometry(handle: HANDLE) -> Result<NTFS_VOLUME_DATA_BUFFER> {
    let mut data = NTFS_VOLUME_DATA_BUFFER::default();
    let mut returned = 0u32;
    unsafe {
        DeviceIoControl(
            handle,
            FSCTL_GET_NTFS_VOLUME_DATA,
            None,
            0,
            Some((&mut data as *mut NTFS_VOLUME_DATA_BUFFER).cast()),
            std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            Some(&mut returned),
            None,
        )
    }
    .map_err(|error| RawVolumeError::Win32("FSCTL_GET_NTFS_VOLUME_DATA", error))?;
    if returned < std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32 {
        return Err(RawVolumeError::Geometry("short NTFS volume data response"));
    }
    Ok(data)
}

fn cross_check_geometry(
    boot: &NtfsGeometry,
    ioctl: &NTFS_VOLUME_DATA_BUFFER,
    expected_serial: u32,
) -> Result<()> {
    let checks = boot.bytes_per_sector == ioctl.BytesPerSector
        && boot.bytes_per_cluster == ioctl.BytesPerCluster
        && boot.bytes_per_file_record == ioctl.BytesPerFileRecordSegment
        && boot.total_sectors == positive_i64(ioctl.NumberSectors, "negative sector count")?
        && boot.mft_start_lcn == positive_i64(ioctl.MftStartLcn, "negative MFT LCN")?
        && boot.mft_mirror_lcn == positive_i64(ioctl.Mft2StartLcn, "negative MFT mirror LCN")?
        && boot.volume_serial == ioctl.VolumeSerialNumber as u64
        && boot.volume_serial as u32 == expected_serial;
    if checks {
        Ok(())
    } else {
        Err(RawVolumeError::Geometry(
            "boot sector, FSCTL and mount serial disagree",
        ))
    }
}

fn positive_i64(value: i64, context: &'static str) -> Result<u64> {
    u64::try_from(value).map_err(|_| RawVolumeError::Geometry(context))
}

fn wide_nul(value: &OsStr) -> Result<Vec<u16>> {
    let mut wide: Vec<u16> = value.encode_wide().collect();
    if wide.is_empty() || wide.len() >= MAX_WIDE_PATH || wide.contains(&0) {
        return Err(RawVolumeError::InvalidPath(
            "empty, oversized or embedded NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn truncate_at_nul(value: &mut Vec<u16>) {
    if let Some(index) = value.iter().position(|unit| *unit == 0) {
        value.truncate(index);
    }
}

trait Utf16AsciiEq {
    fn eq_ignore_ascii_case(&self, other: &[u16]) -> bool;
}

impl Utf16AsciiEq for [u16] {
    fn eq_ignore_ascii_case(&self, other: &[u16]) -> bool {
        self.len() == other.len()
            && self.iter().zip(other).all(|(left, right)| {
                u8::try_from(*left)
                    .ok()
                    .zip(u8::try_from(*right).ok())
                    .is_some_and(|(left, right)| left.eq_ignore_ascii_case(&right))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::Utf16AsciiEq as _;

    #[test]
    fn filesystem_comparison_is_ascii_case_insensitive_only() {
        assert!("NtFs"
            .encode_utf16()
            .collect::<Vec<_>>()
            .eq_ignore_ascii_case(&"NTFS".encode_utf16().collect::<Vec<_>>()));
        assert!(![0xd800u16].eq_ignore_ascii_case(&[0xd800u16]));
    }
}
