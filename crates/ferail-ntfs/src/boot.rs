use crate::{ByteReader, ErrorKind, NtfsError, Result};

const BOOT_SECTOR_PREFIX: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NtfsGeometry {
    pub bytes_per_sector: u32,
    pub sectors_per_cluster: u32,
    pub bytes_per_cluster: u32,
    pub bytes_per_file_record: u32,
    pub total_sectors: u64,
    pub mft_start_lcn: u64,
    pub mft_mirror_lcn: u64,
    pub volume_serial: u64,
}

pub fn parse_boot_sector(reader: &impl ByteReader) -> Result<NtfsGeometry> {
    let mut bytes = [0u8; BOOT_SECTOR_PREFIX];
    reader.read_exact_at(0, &mut bytes)?;

    if &bytes[3..11] != b"NTFS    " || bytes[510..512] != [0x55, 0xaa] {
        return Err(NtfsError::new(
            ErrorKind::InvalidSignature,
            3,
            "NTFS OEM id or boot signature",
        ));
    }

    let bytes_per_sector = u16_at(&bytes, 11) as u32;
    let sectors_per_cluster = bytes[13] as u32;
    if !(512..=4096).contains(&bytes_per_sector) || !bytes_per_sector.is_power_of_two() {
        return Err(NtfsError::new(
            ErrorKind::InvalidGeometry,
            11,
            "bytes per sector",
        ));
    }
    if sectors_per_cluster == 0 || !sectors_per_cluster.is_power_of_two() {
        return Err(NtfsError::new(
            ErrorKind::InvalidGeometry,
            13,
            "sectors per cluster",
        ));
    }
    let bytes_per_cluster = bytes_per_sector
        .checked_mul(sectors_per_cluster)
        .filter(|value| *value <= 2 * 1024 * 1024)
        .ok_or_else(|| {
            NtfsError::new(
                ErrorKind::InvalidGeometry,
                13,
                "cluster byte size overflow or unsupported size",
            )
        })?;

    let total_sectors = u64_at(&bytes, 40);
    let mft_start_lcn = u64_at(&bytes, 48);
    let mft_mirror_lcn = u64_at(&bytes, 56);
    if total_sectors == 0 {
        return Err(NtfsError::new(
            ErrorKind::InvalidGeometry,
            40,
            "zero total sectors",
        ));
    }
    let total_clusters = total_sectors / u64::from(sectors_per_cluster);
    if total_clusters == 0 || mft_start_lcn >= total_clusters || mft_mirror_lcn >= total_clusters {
        return Err(NtfsError::new(
            ErrorKind::InvalidGeometry,
            48,
            "MFT LCN outside volume",
        ));
    }

    let record_code = bytes[64] as i8;
    let bytes_per_file_record = match record_code {
        1..=127 => bytes_per_cluster.checked_mul(record_code as u32),
        -31..=-1 => 1u32.checked_shl((-record_code) as u32),
        _ => None,
    }
    .filter(|value| *value >= bytes_per_sector && value.is_power_of_two())
    .ok_or_else(|| NtfsError::new(ErrorKind::InvalidGeometry, 64, "file record size code"))?;

    let volume_bytes = total_sectors
        .checked_mul(u64::from(bytes_per_sector))
        .ok_or_else(|| NtfsError::new(ErrorKind::Overflow, 40, "volume byte length overflow"))?;
    let mft_offset = mft_start_lcn
        .checked_mul(u64::from(bytes_per_cluster))
        .ok_or_else(|| NtfsError::new(ErrorKind::Overflow, 48, "MFT offset overflow"))?;
    if mft_offset >= volume_bytes {
        return Err(NtfsError::new(
            ErrorKind::InvalidGeometry,
            48,
            "MFT byte offset outside volume",
        ));
    }

    Ok(NtfsGeometry {
        bytes_per_sector,
        sectors_per_cluster,
        bytes_per_cluster,
        bytes_per_file_record,
        total_sectors,
        mft_start_lcn,
        mft_mirror_lcn,
        volume_serial: u64_at(&bytes, 72),
    })
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("fixed field"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SliceReader;

    fn boot_sector() -> [u8; 512] {
        let mut bytes = [0u8; 512];
        bytes[3..11].copy_from_slice(b"NTFS    ");
        bytes[11..13].copy_from_slice(&512u16.to_le_bytes());
        bytes[13] = 8;
        bytes[40..48].copy_from_slice(&2_000_000u64.to_le_bytes());
        bytes[48..56].copy_from_slice(&4u64.to_le_bytes());
        bytes[56..64].copy_from_slice(&8u64.to_le_bytes());
        bytes[64] = (-10i8) as u8;
        bytes[72..80].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
        bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
        bytes
    }

    #[test]
    fn parses_valid_geometry_and_negative_record_exponent() {
        let bytes = boot_sector();
        let geometry = parse_boot_sector(&SliceReader::new(&bytes)).unwrap();
        assert_eq!(geometry.bytes_per_cluster, 4096);
        assert_eq!(geometry.bytes_per_file_record, 1024);
        assert_eq!(geometry.mft_start_lcn, 4);
    }

    #[test]
    fn rejects_invalid_and_overflowing_geometry() {
        let mut bytes = boot_sector();
        bytes[13] = 3;
        assert_eq!(
            parse_boot_sector(&SliceReader::new(&bytes))
                .unwrap_err()
                .kind,
            ErrorKind::InvalidGeometry
        );

        let mut bytes = boot_sector();
        bytes[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            parse_boot_sector(&SliceReader::new(&bytes))
                .unwrap_err()
                .kind,
            ErrorKind::Overflow
        );
    }
}
