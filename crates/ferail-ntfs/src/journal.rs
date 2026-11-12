//! Platform-neutral USN change-journal logic for keeping a cached
//! [`crate::CompactNtfsIndex`] fresh.
//!
//! The Windows helper performs the `FSCTL_QUERY_USN_JOURNAL` and
//! `FSCTL_READ_USN_JOURNAL` calls; everything that decides what those results
//! mean lives here so it can be tested from byte fixtures:
//!
//! - [`parse_usn_read_output`] decodes one `FSCTL_READ_USN_JOURNAL` output
//!   buffer (`USN_RECORD_V2` / `V3`), bounds-checking every record;
//! - [`freshness`] decides whether a cached index is current, can be brought
//!   current by replaying a journal range, or must be rebuilt;
//! - [`ReplayCollector`] turns replayed entries into the set of MFT records
//!   to re-read plus the next replay start.
//!
//! Settling: raw MFT reads may observe a record before NTFS has flushed its
//! latest version, so the journal position a cache advances to is held back
//! to the oldest entry younger than the settle window. Those records are
//! re-read again on the next request until they age out, which turns a
//! possibly stale read into a slower but correct one.

use std::collections::BTreeSet;

use crate::{ErrorKind, FileReference, NtfsError, Result};

/// The root directory record; the only metadata record the index keeps.
pub const ROOT_DIRECTORY_RECORD: u64 = 5;
/// First record number NTFS hands to user files.
pub const FIRST_USER_RECORD: u64 = 16;
/// 100 ns ticks per second.
pub const TICKS_PER_SECOND: i64 = 10_000_000;

const MIN_V2_RECORD: usize = 60;
const MIN_V3_RECORD: usize = 76;

/// The scan keeps the root plus user records; the journal replay must apply
/// the same filter or it would add metadata files the scan never contained.
pub const fn is_indexed_record(record: u64) -> bool {
    record == ROOT_DIRECTORY_RECORD || record >= FIRST_USER_RECORD
}

/// One decoded change-journal entry. The name is not retained: replay
/// re-reads the record itself, and nothing here may log a file name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsnEntry {
    pub file: FileReference,
    pub parent: FileReference,
    pub usn: i64,
    /// FILETIME ticks (UTC, 100 ns since 1601).
    pub timestamp: i64,
    pub reason: u32,
    pub attributes: u32,
}

/// One `FSCTL_READ_USN_JOURNAL` output buffer.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UsnReadPage {
    /// The USN to pass as `StartUsn` for the next read.
    pub next_usn: i64,
    pub entries: Vec<UsnEntry>,
}

/// Decodes an `FSCTL_READ_USN_JOURNAL` output buffer: an 8-byte next-USN
/// followed by 8-byte-aligned `USN_RECORD_V2`/`V3` records. Any malformed
/// record, unknown major version or 128-bit file id that does not fit an
/// NTFS file reference is an error; the caller must treat that as "rebuild".
pub fn parse_usn_read_output(bytes: &[u8]) -> Result<UsnReadPage> {
    if bytes.len() < 8 {
        return Err(journal_error(0, "USN read output shorter than next USN"));
    }
    let next_usn = i64::from_le_bytes(bytes[..8].try_into().expect("fixed field"));
    let mut entries = Vec::new();
    let mut cursor = 8usize;
    while cursor < bytes.len() {
        let remaining = bytes.len() - cursor;
        if remaining < 8 {
            return Err(journal_error(cursor, "truncated USN record header"));
        }
        let length = u32_at(bytes, cursor)? as usize;
        let major = u16_at(bytes, cursor + 4)?;
        if length < 8 || !length.is_multiple_of(8) || length > remaining {
            return Err(journal_error(cursor, "USN record length"));
        }
        let record = &bytes[cursor..cursor + length];
        let entry = match major {
            2 => {
                if length < MIN_V2_RECORD {
                    return Err(journal_error(cursor, "short USN_RECORD_V2"));
                }
                check_name(record, 56, 58, MIN_V2_RECORD, cursor)?;
                UsnEntry {
                    file: FileReference::from_raw(u64_at(record, 8)?),
                    parent: FileReference::from_raw(u64_at(record, 16)?),
                    usn: i64_at(record, 24)?,
                    timestamp: i64_at(record, 32)?,
                    reason: u32_at(record, 40)?,
                    attributes: u32_at(record, 52)?,
                }
            }
            3 => {
                if length < MIN_V3_RECORD {
                    return Err(journal_error(cursor, "short USN_RECORD_V3"));
                }
                check_name(record, 72, 74, MIN_V3_RECORD, cursor)?;
                UsnEntry {
                    file: file_id_128(record, 8, cursor)?,
                    parent: file_id_128(record, 24, cursor)?,
                    usn: i64_at(record, 40)?,
                    timestamp: i64_at(record, 48)?,
                    reason: u32_at(record, 56)?,
                    attributes: u32_at(record, 68)?,
                }
            }
            _ => return Err(journal_error(cursor, "unsupported USN record version")),
        };
        entries.push(entry);
        cursor += length;
    }
    Ok(UsnReadPage { next_usn, entries })
}

/// Journal identity and range at one instant (`USN_JOURNAL_DATA`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JournalState {
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
}

impl JournalState {
    /// The oldest USN that can still be read.
    pub fn oldest_readable(self) -> i64 {
        self.first_usn.max(self.lowest_valid_usn)
    }
}

/// Where a cached index stands relative to the journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JournalCursor {
    pub journal_id: u64,
    /// Every change below this USN has been read from the journal.
    pub synced_usn: i64,
    /// Replay must restart here (`<= synced_usn`): entries from this USN on
    /// were younger than the settle window and are re-read again.
    pub replay_from: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FreshnessReason {
    NoJournal,
    JournalReplaced,
    JournalWentBackwards,
    EntriesPurged,
    InvalidCursor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Freshness {
    /// Nothing changed and nothing is left to settle: reuse as is.
    Current,
    /// Replay `[from, to)` through the overlay, then reuse.
    Replay {
        from: i64,
        to: i64,
    },
    Rebuild(FreshnessReason),
}

/// The reuse rule. A cache is reused only when the journal proves nothing
/// changed, or when every change since it was built can still be read.
pub fn freshness(cursor: JournalCursor, now: Option<JournalState>) -> Freshness {
    let Some(now) = now else {
        return Freshness::Rebuild(FreshnessReason::NoJournal);
    };
    if now.journal_id != cursor.journal_id {
        return Freshness::Rebuild(FreshnessReason::JournalReplaced);
    }
    if cursor.replay_from > cursor.synced_usn || cursor.replay_from < 0 {
        return Freshness::Rebuild(FreshnessReason::InvalidCursor);
    }
    if now.next_usn < cursor.synced_usn {
        return Freshness::Rebuild(FreshnessReason::JournalWentBackwards);
    }
    if cursor.replay_from < now.oldest_readable() {
        return Freshness::Rebuild(FreshnessReason::EntriesPurged);
    }
    if now.next_usn == cursor.synced_usn && cursor.replay_from == cursor.synced_usn {
        return Freshness::Current;
    }
    Freshness::Replay {
        from: cursor.replay_from,
        to: now.next_usn,
    }
}

/// Accumulates replayed entries into the records to re-read.
#[derive(Clone, Debug)]
pub struct ReplayCollector {
    records: BTreeSet<u64>,
    settle_cutoff: i64,
    earliest_unsettled: Option<i64>,
    max_records: usize,
    overflowed: bool,
}

/// What a replay must do: re-read `records` (sorted, indexed records only),
/// then store `next_replay_from` in the cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayPlan {
    pub records: Vec<u64>,
    pub next_replay_from: i64,
}

impl ReplayCollector {
    /// `settle_cutoff` is a FILETIME: entries at or after it are unsettled.
    pub fn new(settle_cutoff: i64, max_records: usize) -> Self {
        Self {
            records: BTreeSet::new(),
            settle_cutoff,
            earliest_unsettled: None,
            max_records,
            overflowed: false,
        }
    }

    pub fn observe(&mut self, entry: &UsnEntry) {
        if entry.timestamp >= self.settle_cutoff {
            self.earliest_unsettled = Some(
                self.earliest_unsettled
                    .map_or(entry.usn, |earliest| earliest.min(entry.usn)),
            );
        }
        if !is_indexed_record(entry.file.record) {
            return;
        }
        if self.records.len() >= self.max_records && !self.records.contains(&entry.file.record) {
            self.overflowed = true;
            return;
        }
        self.records.insert(entry.file.record);
    }

    /// True once more distinct records were seen than the cap; a full
    /// rebuild is then cheaper than re-reading them one by one.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// `None` when the cap overflowed.
    pub fn finish(self, synced_usn: i64) -> Option<ReplayPlan> {
        if self.overflowed {
            return None;
        }
        Some(ReplayPlan {
            records: self.records.into_iter().collect(),
            next_replay_from: self
                .earliest_unsettled
                .map_or(synced_usn, |earliest| earliest.min(synced_usn)),
        })
    }
}

/// Converts Unix seconds to a FILETIME tick count, for settle cutoffs.
pub fn unix_seconds_to_filetime(seconds: i64) -> i64 {
    seconds
        .saturating_add(11_644_473_600)
        .saturating_mul(TICKS_PER_SECOND)
}

fn check_name(
    record: &[u8],
    length_at: usize,
    offset_at: usize,
    header: usize,
    cursor: usize,
) -> Result<()> {
    let length = usize::from(u16_at(record, length_at)?);
    let offset = usize::from(u16_at(record, offset_at)?);
    if length % 2 != 0
        || offset < header
        || offset
            .checked_add(length)
            .is_none_or(|end| end > record.len())
    {
        return Err(journal_error(cursor, "USN record name outside record"));
    }
    Ok(())
}

fn file_id_128(record: &[u8], offset: usize, cursor: usize) -> Result<FileReference> {
    let low = u64_at(record, offset)?;
    let high = u64_at(record, offset + 8)?;
    if high != 0 {
        return Err(journal_error(cursor, "128-bit file id outside NTFS range"));
    }
    Ok(FileReference::from_raw(low))
}

fn journal_error(offset: usize, context: &'static str) -> NtfsError {
    NtfsError::new(ErrorKind::InvalidAttribute, offset as u64, context)
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    bytes
        .get(offset..offset + 2)
        .map(|field| u16::from_le_bytes(field.try_into().expect("fixed field")))
        .ok_or_else(|| journal_error(offset, "truncated u16"))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    bytes
        .get(offset..offset + 4)
        .map(|field| u32::from_le_bytes(field.try_into().expect("fixed field")))
        .ok_or_else(|| journal_error(offset, "truncated u32"))
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<u64> {
    bytes
        .get(offset..offset + 8)
        .map(|field| u64::from_le_bytes(field.try_into().expect("fixed field")))
        .ok_or_else(|| journal_error(offset, "truncated u64"))
}

fn i64_at(bytes: &[u8], offset: usize) -> Result<i64> {
    Ok(u64_at(bytes, offset)? as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2(file: u64, parent: u64, usn: i64, timestamp: i64, name: &str) -> Vec<u8> {
        let name: Vec<u16> = name.encode_utf16().collect();
        let length = (60 + name.len() * 2 + 7) & !7;
        let mut out = vec![0u8; length];
        out[0..4].copy_from_slice(&(length as u32).to_le_bytes());
        out[4..6].copy_from_slice(&2u16.to_le_bytes());
        out[8..16].copy_from_slice(&file.to_le_bytes());
        out[16..24].copy_from_slice(&parent.to_le_bytes());
        out[24..32].copy_from_slice(&usn.to_le_bytes());
        out[32..40].copy_from_slice(&timestamp.to_le_bytes());
        out[40..44].copy_from_slice(&0x100u32.to_le_bytes());
        out[52..56].copy_from_slice(&0x20u32.to_le_bytes());
        out[56..58].copy_from_slice(&((name.len() * 2) as u16).to_le_bytes());
        out[58..60].copy_from_slice(&60u16.to_le_bytes());
        for (index, unit) in name.iter().enumerate() {
            out[60 + index * 2..62 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        out
    }

    fn v3(file: u64, high: u64, usn: i64) -> Vec<u8> {
        let mut out = vec![0u8; 80];
        out[0..4].copy_from_slice(&80u32.to_le_bytes());
        out[4..6].copy_from_slice(&3u16.to_le_bytes());
        out[8..16].copy_from_slice(&file.to_le_bytes());
        out[16..24].copy_from_slice(&high.to_le_bytes());
        out[24..32].copy_from_slice(&5u64.to_le_bytes());
        out[40..48].copy_from_slice(&usn.to_le_bytes());
        out[72..74].copy_from_slice(&2u16.to_le_bytes());
        out[74..76].copy_from_slice(&76u16.to_le_bytes());
        out
    }

    fn page(next: i64, records: &[Vec<u8>]) -> Vec<u8> {
        let mut out = next.to_le_bytes().to_vec();
        for record in records {
            out.extend_from_slice(record);
        }
        out
    }

    #[test]
    fn parses_v2_and_v3_records_without_keeping_names() {
        let file = (3u64 << 48) | 40;
        let bytes = page(
            900,
            &[
                v2(file, (1u64 << 48) | 5, 100, 7, "secret.txt"),
                v3(41, 0, 200),
            ],
        );
        let parsed = parse_usn_read_output(&bytes).unwrap();
        assert_eq!(parsed.next_usn, 900);
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(
            parsed.entries[0].file,
            FileReference {
                record: 40,
                sequence: 3
            }
        );
        assert_eq!(parsed.entries[0].parent.record, 5);
        assert_eq!(parsed.entries[0].usn, 100);
        assert_eq!(parsed.entries[0].timestamp, 7);
        assert_eq!(parsed.entries[0].attributes, 0x20);
        assert_eq!(parsed.entries[1].file.record, 41);
        assert_eq!(parsed.entries[1].usn, 200);
    }

    #[test]
    fn empty_page_is_only_a_next_usn() {
        let parsed = parse_usn_read_output(&page(5, &[])).unwrap();
        assert_eq!(
            parsed,
            UsnReadPage {
                next_usn: 5,
                entries: Vec::new()
            }
        );
        assert!(parse_usn_read_output(&[0; 7]).is_err());
    }

    #[test]
    fn malformed_records_are_rejected() {
        let good = v2(40, 5, 1, 1, "a");
        let mutate = |f: &dyn Fn(&mut Vec<u8>)| {
            let mut record = good.clone();
            f(&mut record);
            parse_usn_read_output(&page(0, &[record])).unwrap_err().kind
        };
        // Length beyond buffer, unaligned, below header, unknown version,
        // name outside record.
        assert_eq!(
            mutate(&|r| r[0..4].copy_from_slice(&1000u32.to_le_bytes())),
            ErrorKind::InvalidAttribute
        );
        assert!(parse_usn_read_output(&page(
            0,
            &[{
                let mut r = good.clone();
                r[0..4].copy_from_slice(&61u32.to_le_bytes());
                r
            }]
        ))
        .is_err());
        assert!(parse_usn_read_output(&page(
            0,
            &[{
                let mut r = good.clone();
                r[0..4].copy_from_slice(&0u32.to_le_bytes());
                r
            }]
        ))
        .is_err());
        assert!(parse_usn_read_output(&page(
            0,
            &[{
                let mut r = good.clone();
                r[4..6].copy_from_slice(&4u16.to_le_bytes());
                r
            }]
        ))
        .is_err());
        assert!(parse_usn_read_output(&page(
            0,
            &[{
                let mut r = good.clone();
                r[56..58].copy_from_slice(&200u16.to_le_bytes());
                r
            }]
        ))
        .is_err());
        // Truncated trailing header.
        let mut bytes = page(0, std::slice::from_ref(&good));
        bytes.extend_from_slice(&[1, 2, 3]);
        assert!(parse_usn_read_output(&bytes).is_err());
        // A 128-bit id outside the 64-bit NTFS reference space.
        assert!(parse_usn_read_output(&page(0, &[v3(41, 1, 1)])).is_err());
    }

    fn state(id: u64, first: i64, next: i64) -> Option<JournalState> {
        Some(JournalState {
            journal_id: id,
            first_usn: first,
            next_usn: next,
            lowest_valid_usn: 0,
        })
    }

    fn cursor(synced: i64, replay_from: i64) -> JournalCursor {
        JournalCursor {
            journal_id: 7,
            synced_usn: synced,
            replay_from,
        }
    }

    #[test]
    fn freshness_reuses_only_an_unchanged_settled_journal() {
        assert_eq!(
            freshness(cursor(500, 500), state(7, 0, 500)),
            Freshness::Current
        );
        assert_eq!(
            freshness(cursor(500, 500), state(7, 0, 800)),
            Freshness::Replay { from: 500, to: 800 }
        );
        // Unsettled entries are replayed even when nothing new arrived.
        assert_eq!(
            freshness(cursor(500, 300), state(7, 0, 500)),
            Freshness::Replay { from: 300, to: 500 }
        );
    }

    #[test]
    fn freshness_rebuilds_on_every_unprovable_case() {
        assert_eq!(
            freshness(cursor(500, 500), None),
            Freshness::Rebuild(FreshnessReason::NoJournal)
        );
        assert_eq!(
            freshness(cursor(500, 500), state(8, 0, 500)),
            Freshness::Rebuild(FreshnessReason::JournalReplaced)
        );
        assert_eq!(
            freshness(cursor(500, 500), state(7, 0, 400)),
            Freshness::Rebuild(FreshnessReason::JournalWentBackwards)
        );
        // The journal wrapped past the replay start: entries were purged.
        assert_eq!(
            freshness(cursor(500, 300), state(7, 301, 900)),
            Freshness::Rebuild(FreshnessReason::EntriesPurged)
        );
        let mut lowest = state(7, 0, 900).unwrap();
        lowest.lowest_valid_usn = 400;
        assert_eq!(
            freshness(cursor(500, 300), Some(lowest)),
            Freshness::Rebuild(FreshnessReason::EntriesPurged)
        );
        assert_eq!(
            freshness(cursor(500, 600), state(7, 0, 900)),
            Freshness::Rebuild(FreshnessReason::InvalidCursor)
        );
    }

    fn entry(record: u64, usn: i64, timestamp: i64) -> UsnEntry {
        UsnEntry {
            file: FileReference {
                record,
                sequence: 1,
            },
            parent: FileReference {
                record: 5,
                sequence: 5,
            },
            usn,
            timestamp,
            reason: 0,
            attributes: 0,
        }
    }

    #[test]
    fn collector_dedups_filters_metadata_and_holds_back_unsettled_entries() {
        let mut collector = ReplayCollector::new(1000, 10);
        for e in [
            entry(40, 10, 1),
            entry(40, 20, 2),
            entry(3, 30, 3),
            entry(5, 40, 4),
            entry(41, 50, 1000),
            entry(42, 60, 2000),
        ] {
            collector.observe(&e);
        }
        let plan = collector.finish(70).unwrap();
        assert_eq!(plan.records, vec![5, 40, 41, 42]);
        assert_eq!(plan.next_replay_from, 50);

        let mut settled = ReplayCollector::new(1000, 10);
        settled.observe(&entry(40, 10, 1));
        assert_eq!(settled.finish(70).unwrap().next_replay_from, 70);
    }

    #[test]
    fn collector_overflow_means_rebuild() {
        let mut collector = ReplayCollector::new(i64::MAX, 2);
        collector.observe(&entry(40, 1, 0));
        collector.observe(&entry(41, 2, 0));
        collector.observe(&entry(40, 3, 0));
        assert!(!collector.overflowed());
        collector.observe(&entry(42, 4, 0));
        assert!(collector.overflowed());
        assert!(collector.finish(5).is_none());
    }

    #[test]
    fn filetime_conversion_round_trips_the_epoch() {
        assert_eq!(unix_seconds_to_filetime(0), 116_444_736_000_000_000);
        assert_eq!(
            crate::ntfs_ticks_to_unix_seconds(unix_seconds_to_filetime(1_700_000_000) as u64),
            Some(1_700_000_000)
        );
    }
}
