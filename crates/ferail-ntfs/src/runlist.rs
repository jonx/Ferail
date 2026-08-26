use crate::{ErrorKind, NtfsError, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataRun {
    pub vcn: u64,
    pub cluster_count: u64,
    /// `None` is a sparse run. Otherwise this is the absolute starting LCN.
    pub lcn: Option<u64>,
}

pub fn parse_mapping_pairs(bytes: &[u8], start_vcn: u64) -> Result<Vec<DataRun>> {
    let mut cursor = 0usize;
    let mut vcn = start_vcn;
    let mut previous_lcn = 0i64;
    let mut runs = Vec::new();

    loop {
        let header = *bytes.get(cursor).ok_or_else(|| {
            NtfsError::new(ErrorKind::Truncated, cursor as u64, "runlist terminator")
        })?;
        cursor += 1;
        if header == 0 {
            return Ok(runs);
        }

        let length_bytes = usize::from(header & 0x0f);
        let offset_bytes = usize::from(header >> 4);
        if length_bytes == 0 || length_bytes > 8 || offset_bytes > 8 {
            return Err(NtfsError::new(
                ErrorKind::InvalidRunlist,
                (cursor - 1) as u64,
                "mapping-pair field width",
            ));
        }
        let end = cursor
            .checked_add(length_bytes)
            .and_then(|value| value.checked_add(offset_bytes))
            .ok_or_else(|| {
                NtfsError::new(
                    ErrorKind::Overflow,
                    cursor as u64,
                    "mapping-pair cursor overflow",
                )
            })?;
        let pair = bytes.get(cursor..end).ok_or_else(|| {
            NtfsError::new(ErrorKind::Truncated, cursor as u64, "mapping-pair payload")
        })?;

        let cluster_count = decode_unsigned(&pair[..length_bytes]);
        if cluster_count == 0 {
            return Err(NtfsError::new(
                ErrorKind::InvalidRunlist,
                cursor as u64,
                "zero-length run",
            ));
        }
        let lcn = if offset_bytes == 0 {
            None
        } else {
            let delta = decode_signed(&pair[length_bytes..]);
            previous_lcn = previous_lcn.checked_add(delta).ok_or_else(|| {
                NtfsError::new(ErrorKind::Overflow, cursor as u64, "LCN delta overflow")
            })?;
            Some(u64::try_from(previous_lcn).map_err(|_| {
                NtfsError::new(
                    ErrorKind::InvalidRunlist,
                    cursor as u64,
                    "negative absolute LCN",
                )
            })?)
        };

        runs.push(DataRun {
            vcn,
            cluster_count,
            lcn,
        });
        vcn = vcn
            .checked_add(cluster_count)
            .ok_or_else(|| NtfsError::new(ErrorKind::Overflow, cursor as u64, "VCN overflow"))?;
        cursor = end;
    }
}

fn decode_unsigned(bytes: &[u8]) -> u64 {
    bytes.iter().enumerate().fold(0u64, |value, (shift, byte)| {
        value | (u64::from(*byte) << (shift * 8))
    })
}

fn decode_signed(bytes: &[u8]) -> i64 {
    let unsigned = decode_unsigned(bytes);
    let bits = bytes.len() * 8;
    if bits == 64 || unsigned & (1u64 << (bits - 1)) == 0 {
        unsigned as i64
    } else {
        (unsigned | (!0u64 << bits)) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fragmented_negative_delta_and_sparse_runs() {
        // +100 LCN for 3 clusters, -4 LCN for 2, then 5 sparse clusters.
        let runs = parse_mapping_pairs(&[0x11, 3, 100, 0x11, 2, 0xfc, 0x01, 5, 0], 0).unwrap();
        assert_eq!(
            runs,
            vec![
                DataRun {
                    vcn: 0,
                    cluster_count: 3,
                    lcn: Some(100)
                },
                DataRun {
                    vcn: 3,
                    cluster_count: 2,
                    lcn: Some(96)
                },
                DataRun {
                    vcn: 5,
                    cluster_count: 5,
                    lcn: None
                }
            ]
        );
    }

    #[test]
    fn rejects_truncation_negative_lcn_and_vcn_overflow() {
        assert_eq!(
            parse_mapping_pairs(&[0x21, 1, 1], 0).unwrap_err().kind,
            ErrorKind::Truncated
        );
        assert_eq!(
            parse_mapping_pairs(&[0x11, 1, 0xff, 0], 0)
                .unwrap_err()
                .kind,
            ErrorKind::InvalidRunlist
        );
        assert_eq!(
            parse_mapping_pairs(&[0x01, 2, 0], u64::MAX)
                .unwrap_err()
                .kind,
            ErrorKind::Overflow
        );
    }
}
