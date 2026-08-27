use std::collections::HashSet;

use crate::{
    AttributeList, ByteReader, DataRun, ErrorKind, FileRecord, FileReference, NtfsError,
    NtfsGeometry, RecordParseOptions, Result, parse_attribute_list_entries, parse_file_record,
};

const ATTRIBUTE_DATA: u32 = 0x80;
const MAX_MFT_EXTENSIONS: usize = 1024;
const MAX_ATTRIBUTE_LIST_BYTES: u64 = 16 * 1024 * 1024;

/// A logical non-resident NTFS stream mapped onto a random-access volume.
/// Runs are sorted and validated once; reads binary-search the relevant VCN
/// and never allocate in proportion to the stream length.
#[derive(Debug)]
pub struct MappedStream<'a, R: ByteReader> {
    source: &'a R,
    runs: Vec<DataRun>,
    cluster_bytes: u64,
    logical_bytes: u64,
}

impl<'a, R: ByteReader> MappedStream<'a, R> {
    pub fn new(
        source: &'a R,
        mut runs: Vec<DataRun>,
        cluster_bytes: u64,
        logical_bytes: u64,
    ) -> Result<Self> {
        if cluster_bytes == 0 || !cluster_bytes.is_power_of_two() {
            return Err(NtfsError::new(
                ErrorKind::InvalidGeometry,
                0,
                "mapped-stream cluster size",
            ));
        }
        runs.sort_unstable_by_key(|run| run.vcn);
        let mut previous_end = 0u64;
        for run in &runs {
            if run.cluster_count == 0 || run.vcn < previous_end {
                return Err(NtfsError::new(
                    ErrorKind::InvalidRunlist,
                    run.vcn,
                    "overlapping or empty mapped-stream run",
                ));
            }
            previous_end = run.vcn.checked_add(run.cluster_count).ok_or_else(|| {
                NtfsError::new(ErrorKind::Overflow, run.vcn, "mapped-stream VCN overflow")
            })?;
            if let Some(lcn) = run.lcn {
                let last_lcn = lcn.checked_add(run.cluster_count).ok_or_else(|| {
                    NtfsError::new(ErrorKind::Overflow, lcn, "mapped-stream LCN overflow")
                })?;
                let end = last_lcn.checked_mul(cluster_bytes).ok_or_else(|| {
                    NtfsError::new(ErrorKind::Overflow, lcn, "mapped-stream byte overflow")
                })?;
                if end > source.len() {
                    return Err(NtfsError::new(
                        ErrorKind::InvalidRunlist,
                        lcn,
                        "mapped run outside source",
                    ));
                }
            }
        }
        Ok(Self {
            source,
            runs,
            cluster_bytes,
            logical_bytes,
        })
    }

    pub fn runs(&self) -> &[DataRun] {
        &self.runs
    }

    pub fn validate_complete_non_sparse(&self) -> Result<()> {
        let required_clusters = self
            .logical_bytes
            .checked_add(self.cluster_bytes - 1)
            .ok_or_else(|| {
                NtfsError::new(ErrorKind::Overflow, 0, "stream cluster rounding overflow")
            })?
            / self.cluster_bytes;
        let mut expected_vcn = 0u64;
        for run in &self.runs {
            if run.vcn != expected_vcn || run.lcn.is_none() {
                return Err(NtfsError::new(
                    ErrorKind::InvalidRunlist,
                    expected_vcn,
                    "MFT stream gap or sparse run",
                ));
            }
            expected_vcn = expected_vcn.checked_add(run.cluster_count).ok_or_else(|| {
                NtfsError::new(ErrorKind::Overflow, run.vcn, "MFT coverage overflow")
            })?;
            if expected_vcn >= required_clusters {
                return Ok(());
            }
        }
        Err(NtfsError::new(
            ErrorKind::Truncated,
            expected_vcn,
            "MFT runlist does not cover valid length",
        ))
    }

    fn run_for_vcn(&self, vcn: u64) -> Option<DataRun> {
        let insertion = self.runs.partition_point(|run| run.vcn <= vcn);
        let run = *self.runs.get(insertion.checked_sub(1)?)?;
        (vcn < run.vcn.checked_add(run.cluster_count)?).then_some(run)
    }
}

impl<R: ByteReader> ByteReader for MappedStream<'_, R> {
    fn len(&self) -> u64 {
        self.logical_bytes
    }

    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(destination.len() as u64)
            .ok_or_else(|| NtfsError::new(ErrorKind::Overflow, offset, "stream range overflow"))?;
        if end > self.logical_bytes {
            return Err(NtfsError::new(
                ErrorKind::Truncated,
                offset,
                "read beyond logical stream",
            ));
        }

        let mut logical = offset;
        let mut written = 0usize;
        while written < destination.len() {
            let vcn = logical / self.cluster_bytes;
            let within_cluster = logical % self.cluster_bytes;
            let run = self.run_for_vcn(vcn).ok_or_else(|| {
                NtfsError::new(ErrorKind::InvalidRunlist, vcn, "unmapped stream VCN")
            })?;
            let clusters_left = run
                .vcn
                .checked_add(run.cluster_count)
                .and_then(|end| end.checked_sub(vcn))
                .ok_or_else(|| {
                    NtfsError::new(ErrorKind::Overflow, vcn, "run remainder overflow")
                })?;
            let bytes_left = clusters_left
                .checked_mul(self.cluster_bytes)
                .and_then(|bytes| bytes.checked_sub(within_cluster))
                .ok_or_else(|| {
                    NtfsError::new(ErrorKind::Overflow, logical, "run byte remainder overflow")
                })?;
            let count = usize::try_from(bytes_left.min((destination.len() - written) as u64))
                .map_err(|_| {
                    NtfsError::new(ErrorKind::Overflow, logical, "mapped read size overflow")
                })?;
            if let Some(run_lcn) = run.lcn {
                let source_cluster = run_lcn.checked_add(vcn - run.vcn).ok_or_else(|| {
                    NtfsError::new(ErrorKind::Overflow, vcn, "source LCN overflow")
                })?;
                let source_offset = source_cluster
                    .checked_mul(self.cluster_bytes)
                    .and_then(|base| base.checked_add(within_cluster))
                    .ok_or_else(|| {
                        NtfsError::new(ErrorKind::Overflow, vcn, "source byte offset overflow")
                    })?;
                self.source
                    .read_exact_at(source_offset, &mut destination[written..written + count])?;
            } else {
                destination[written..written + count].fill(0);
            }
            written += count;
            logical = logical.checked_add(count as u64).ok_or_else(|| {
                NtfsError::new(ErrorKind::Overflow, logical, "stream cursor overflow")
            })?;
        }
        Ok(())
    }
}

/// Bootstraps `$MFT` from record zero without assuming it is contiguous.
/// Resident attribute-list entries are followed in increasing VCN order; each
/// extension must itself be reachable through the runs already discovered.
pub fn bootstrap_mft<'a, R: ByteReader>(
    volume: &'a R,
    geometry: NtfsGeometry,
    mft_valid_bytes: u64,
) -> Result<MappedStream<'a, R>> {
    if mft_valid_bytes == 0 {
        return Err(NtfsError::new(
            ErrorKind::InvalidGeometry,
            0,
            "zero MFT valid length",
        ));
    }
    let record_bytes = geometry.bytes_per_file_record as usize;
    let direct_offset = geometry
        .mft_start_lcn
        .checked_mul(u64::from(geometry.bytes_per_cluster))
        .ok_or_else(|| NtfsError::new(ErrorKind::Overflow, 0, "record-zero offset overflow"))?;
    let mut buffer = vec![0u8; record_bytes];
    volume.read_exact_at(direct_offset, &mut buffer)?;
    let options = RecordParseOptions {
        bytes_per_sector: geometry.bytes_per_sector as usize,
        expected_record_number: Some(0),
        include_data_runs: true,
    };
    let record_zero = parse_file_record(&buffer, options)?;
    if !record_zero.in_use || record_zero.base_reference.is_some() {
        return Err(NtfsError::new(
            ErrorKind::InvalidAttribute,
            0,
            "invalid MFT base record",
        ));
    }
    let base_reference = FileReference {
        record: 0,
        sequence: record_zero.sequence,
    };
    let mut runs = data_runs(&record_zero);
    let logical_bytes = record_zero
        .data
        .iter()
        .find(|data| data.lowest_vcn == 0)
        .map(|data| data.logical_bytes)
        .ok_or_else(|| NtfsError::new(ErrorKind::InvalidAttribute, 0, "MFT has no unnamed DATA"))?;
    if logical_bytes < mft_valid_bytes {
        return Err(NtfsError::new(
            ErrorKind::Truncated,
            logical_bytes,
            "MFT logical length below valid length",
        ));
    }

    let mut queue =
        extension_references(&record_zero, volume, u64::from(geometry.bytes_per_cluster))?;
    queue.sort_unstable_by_key(|(vcn, reference)| (*vcn, reference.record));
    let mut visited = HashSet::new();
    visited.insert(base_reference);
    let mut cursor = 0usize;
    while cursor < queue.len() {
        if visited.len() > MAX_MFT_EXTENSIONS {
            return Err(NtfsError::new(
                ErrorKind::LimitExceeded,
                visited.len() as u64,
                "too many MFT extension records",
            ));
        }
        let (_, reference) = queue[cursor];
        cursor += 1;
        if !visited.insert(reference) {
            continue;
        }
        let partial = MappedStream::new(
            volume,
            runs.clone(),
            u64::from(geometry.bytes_per_cluster),
            mft_valid_bytes,
        )?;
        let offset = reference
            .record
            .checked_mul(geometry.bytes_per_file_record as u64)
            .ok_or_else(|| {
                NtfsError::new(
                    ErrorKind::Overflow,
                    reference.record,
                    "MFT extension offset overflow",
                )
            })?;
        partial.read_exact_at(offset, &mut buffer)?;
        let extension = parse_file_record(
            &buffer,
            RecordParseOptions {
                bytes_per_sector: geometry.bytes_per_sector as usize,
                expected_record_number: Some(reference.record),
                include_data_runs: true,
            },
        )?;
        if extension.sequence != reference.sequence
            || extension.base_reference != Some(base_reference)
        {
            return Err(NtfsError::new(
                ErrorKind::InvalidAttribute,
                offset,
                "MFT extension identity mismatch",
            ));
        }
        runs.extend(data_runs(&extension));
        queue.extend(extension_references(
            &extension,
            volume,
            u64::from(geometry.bytes_per_cluster),
        )?);
        queue[cursor..].sort_unstable_by_key(|(vcn, reference)| (*vcn, reference.record));
    }

    let stream = MappedStream::new(
        volume,
        runs,
        u64::from(geometry.bytes_per_cluster),
        mft_valid_bytes,
    )?;
    stream.validate_complete_non_sparse()?;
    Ok(stream)
}

fn data_runs(record: &FileRecord) -> Vec<DataRun> {
    record
        .data
        .iter()
        .flat_map(|data| data.runs.iter().copied())
        .collect()
}

fn extension_references<R: ByteReader>(
    record: &FileRecord,
    volume: &R,
    cluster_bytes: u64,
) -> Result<Vec<(u64, FileReference)>> {
    let mut references = Vec::new();
    for list in &record.attribute_lists {
        match list {
            AttributeList::Resident(entries) => references.extend(
                entries
                    .iter()
                    .filter(|entry| entry.attribute_type == ATTRIBUTE_DATA)
                    .map(|entry| (entry.lowest_vcn, entry.record)),
            ),
            AttributeList::NonResident {
                logical_bytes,
                runs,
                ..
            } => {
                if *logical_bytes > MAX_ATTRIBUTE_LIST_BYTES {
                    return Err(NtfsError::new(
                        ErrorKind::LimitExceeded,
                        *logical_bytes,
                        "non-resident ATTRIBUTE_LIST exceeds cap",
                    ));
                }
                let stream =
                    MappedStream::new(volume, runs.clone(), cluster_bytes, *logical_bytes)?;
                let length = usize::try_from(*logical_bytes).map_err(|_| {
                    NtfsError::new(
                        ErrorKind::Overflow,
                        *logical_bytes,
                        "ATTRIBUTE_LIST length does not fit usize",
                    )
                })?;
                let mut bytes = vec![0u8; length];
                stream.read_exact_at(0, &mut bytes)?;
                references.extend(
                    parse_attribute_list_entries(&bytes)?
                        .into_iter()
                        .filter(|entry| entry.attribute_type == ATTRIBUTE_DATA)
                        .map(|entry| (entry.lowest_vcn, entry.record)),
                );
            }
        }
    }
    Ok(references)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SliceReader;

    #[test]
    fn fragmented_stream_reads_across_runs_and_negative_layout_order() {
        let mut source = vec![0u8; 64];
        source[24..32].copy_from_slice(b"abcdefgh");
        source[8..16].copy_from_slice(b"ijklmnop");
        let reader = SliceReader::new(&source);
        let stream = MappedStream::new(
            &reader,
            vec![
                DataRun {
                    vcn: 1,
                    cluster_count: 1,
                    lcn: Some(1),
                },
                DataRun {
                    vcn: 0,
                    cluster_count: 1,
                    lcn: Some(3),
                },
            ],
            8,
            16,
        )
        .unwrap();
        let mut bytes = [0u8; 12];
        stream.read_exact_at(4, &mut bytes).unwrap();
        assert_eq!(&bytes, b"efghijklmnop");
        stream.validate_complete_non_sparse().unwrap();
    }

    #[test]
    fn sparse_general_stream_zero_fills_but_mft_validation_rejects_it() {
        let source = [0u8; 32];
        let reader = SliceReader::new(&source);
        let stream = MappedStream::new(
            &reader,
            vec![DataRun {
                vcn: 0,
                cluster_count: 2,
                lcn: None,
            }],
            8,
            16,
        )
        .unwrap();
        let mut bytes = [1u8; 16];
        stream.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [0u8; 16]);
        assert_eq!(
            stream.validate_complete_non_sparse().unwrap_err().kind,
            ErrorKind::InvalidRunlist
        );
    }

    #[test]
    fn gaps_and_overlaps_are_never_silently_read() {
        let source = [0u8; 64];
        let reader = SliceReader::new(&source);
        let stream = MappedStream::new(
            &reader,
            vec![DataRun {
                vcn: 1,
                cluster_count: 1,
                lcn: Some(1),
            }],
            8,
            16,
        )
        .unwrap();
        let mut byte = [0u8; 1];
        assert_eq!(
            stream.read_exact_at(0, &mut byte).unwrap_err().kind,
            ErrorKind::InvalidRunlist
        );
        assert_eq!(
            MappedStream::new(
                &reader,
                vec![
                    DataRun {
                        vcn: 0,
                        cluster_count: 2,
                        lcn: Some(0)
                    },
                    DataRun {
                        vcn: 1,
                        cluster_count: 1,
                        lcn: Some(3)
                    }
                ],
                8,
                16
            )
            .unwrap_err()
            .kind,
            ErrorKind::InvalidRunlist
        );
    }
}
