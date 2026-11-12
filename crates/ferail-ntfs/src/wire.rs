use crate::search::PATH_SEPARATOR;
use crate::{
    ErrorKind, FileReference, NeutralNodeKind, NeutralRow, NtfsError, Result, SearchEntryKind,
    SearchRow, MAX_SEARCH_TERMS, MAX_SEARCH_TERM_UNITS,
};

const MAGIC: &[u8; 4] = b"FDU1";
/// Version the helper is launched with and that v2 frames carry. Frames of
/// the v1 Disk Usage kinds are still written and accepted as version 1, so
/// their bytes are unchanged; Search, SearchBatch, SearchComplete and
/// Release exist only in version 2.
pub const PROTOCOL_VERSION: u16 = 2;
pub const PROTOCOL_VERSION_V1: u16 = 1;
pub const FRAME_HEADER_BYTES: usize = 20;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_PATH_UNITS: usize = 32_767;
const MAX_BATCH_ROWS: usize = 256;
const SEARCH_ROW_CLOUD: u8 = 0x01;

const KIND_START: u16 = 1;
const KIND_CANCEL: u16 = 2;
const KIND_HELLO: u16 = 3;
const KIND_READY: u16 = 4;
const KIND_BATCH: u16 = 5;
const KIND_PROGRESS: u16 = 6;
const KIND_COMPLETE: u16 = 7;
const KIND_FAILED: u16 = 8;
const KIND_SEARCH: u16 = 9;
const KIND_SEARCH_BATCH: u16 = 10;
const KIND_SEARCH_COMPLETE: u16 = 11;
const KIND_RELEASE: u16 = 12;
const LAST_V1_KIND: u16 = KIND_FAILED;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    pub volume_serial: u64,
    pub file_id: [u8; 16],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SizingMode {
    Apparent,
    Allocated,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartRequest {
    pub volume_guid: Vec<u16>,
    pub root: Vec<u16>,
    pub root_identity: FileIdentity,
    pub sizing_mode: SizingMode,
    pub descend_packages: bool,
    pub root_id: u64,
    pub first_child_id: u64,
}

/// A name search below `root`. The helper validates volume and root identity
/// exactly as for [`StartRequest`]. `terms` are matched as given against the
/// lowercased name or relative path (see [`crate::SearchQuery`]); an empty
/// list matches every visible entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRequest {
    pub volume_guid: Vec<u16>,
    pub root: Vec<u16>,
    pub root_identity: FileIdentity,
    pub terms: Vec<String>,
    pub match_path: bool,
    pub include_hidden: bool,
    pub descend_packages: bool,
}

/// Terminal frame of a successful search.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchCompletion {
    pub matches: u64,
    pub entries_visited: u64,
    pub directories_descended: u64,
    pub skipped_paths: u64,
    pub cycles_skipped: u64,
    pub corrupt_records: u64,
    /// Journal position the index reflects, and the position at the end.
    pub start_journal_id: u64,
    pub start_next_usn: i64,
    pub end_journal_id: u64,
    pub end_next_usn: i64,
    /// The cached index served this request (possibly after a replay).
    pub index_reused: bool,
    /// The volume changed while the search ran.
    pub best_effort_live: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanPhase {
    Opening,
    MappingMft,
    ReadingRecords,
    BuildingIndex,
    Traversing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Progress {
    pub phase: ScanPhase,
    pub completed: u64,
    pub total: u64,
    pub live_records: u64,
    pub corrupt_records: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Completion {
    pub rows: u64,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub corrupt_records: u64,
    pub skipped_records: u64,
    pub start_journal_id: u64,
    pub start_next_usn: i64,
    pub end_journal_id: u64,
    pub end_next_usn: i64,
    pub best_effort_live: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureCode {
    Unsupported,
    AccessDenied,
    Validation,
    Protocol,
    CorruptVolume,
    Timeout,
    Cancelled,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DuMessage {
    Start(StartRequest),
    Cancel,
    Hello {
        helper_pid: u32,
    },
    Ready,
    Batch(Vec<NeutralRow>),
    Progress(Progress),
    Complete(Completion),
    Failed(FailureCode),
    /// Parent to helper (v2): run a name search.
    Search(SearchRequest),
    /// Helper to parent (v2): up to 256 search hits.
    SearchBatch(Vec<SearchRow>),
    /// Helper to parent (v2): terminal frame of a search.
    SearchComplete(SearchCompletion),
    /// Parent to helper (v2), between requests: drop every cached index.
    /// The helper sends no reply.
    Release,
}

impl DuMessage {
    fn wire_version(&self) -> u16 {
        match self {
            Self::Search(_) | Self::SearchBatch(_) | Self::SearchComplete(_) | Self::Release => {
                PROTOCOL_VERSION
            }
            _ => PROTOCOL_VERSION_V1,
        }
    }
}

pub fn encode_frame(request_id: u64, message: &DuMessage) -> Result<Vec<u8>> {
    let (kind, payload) = encode_payload(message)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(protocol_error(0, "frame payload exceeds cap"));
    }
    let payload_length = u32::try_from(payload.len())
        .map_err(|_| protocol_error(0, "frame payload length does not fit u32"))?;
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + payload.len());
    frame.extend_from_slice(MAGIC);
    frame.extend_from_slice(&message.wire_version().to_le_bytes());
    frame.extend_from_slice(&kind.to_le_bytes());
    frame.extend_from_slice(&request_id.to_le_bytes());
    frame.extend_from_slice(&payload_length.to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub fn decode_frame(bytes: &[u8], expected_request_id: Option<u64>) -> Result<(u64, DuMessage)> {
    if bytes.len() < FRAME_HEADER_BYTES {
        return Err(protocol_error(bytes.len(), "truncated frame header"));
    }
    let version = u16_at(bytes, 4)?;
    if &bytes[..4] != MAGIC || !(PROTOCOL_VERSION_V1..=PROTOCOL_VERSION).contains(&version) {
        return Err(protocol_error(0, "bad frame magic or protocol version"));
    }
    let kind = u16_at(bytes, 6)?;
    if version == PROTOCOL_VERSION_V1 && kind > LAST_V1_KIND {
        return Err(protocol_error(6, "frame kind requires protocol version 2"));
    }
    let request_id = u64_at(bytes, 8)?;
    if expected_request_id.is_some_and(|expected| expected != request_id) {
        return Err(protocol_error(8, "mismatched request id"));
    }
    let payload_length = usize::try_from(u32_at(bytes, 16)?)
        .map_err(|_| protocol_error(16, "payload length does not fit usize"))?;
    if payload_length > MAX_FRAME_BYTES {
        return Err(protocol_error(16, "payload exceeds cap"));
    }
    let expected_length = FRAME_HEADER_BYTES
        .checked_add(payload_length)
        .ok_or_else(|| protocol_error(16, "frame length overflow"))?;
    if bytes.len() != expected_length {
        return Err(protocol_error(
            bytes.len(),
            "truncated frame or trailing bytes",
        ));
    }
    let message = decode_payload(kind, &bytes[FRAME_HEADER_BYTES..])?;
    Ok((request_id, message))
}

fn encode_payload(message: &DuMessage) -> Result<(u16, Vec<u8>)> {
    let mut out = Vec::new();
    let kind = match message {
        DuMessage::Start(start) => {
            write_utf16(&mut out, &start.volume_guid, "volume GUID")?;
            write_utf16(&mut out, &start.root, "root path")?;
            out.extend_from_slice(&start.root_identity.volume_serial.to_le_bytes());
            out.extend_from_slice(&start.root_identity.file_id);
            out.push(match start.sizing_mode {
                SizingMode::Apparent => 0,
                SizingMode::Allocated => 1,
            });
            out.push(u8::from(start.descend_packages));
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(&start.root_id.to_le_bytes());
            out.extend_from_slice(&start.first_child_id.to_le_bytes());
            KIND_START
        }
        DuMessage::Cancel => KIND_CANCEL,
        DuMessage::Hello { helper_pid } => {
            out.extend_from_slice(&helper_pid.to_le_bytes());
            KIND_HELLO
        }
        DuMessage::Ready => KIND_READY,
        DuMessage::Batch(rows) => {
            if rows.is_empty() || rows.len() > MAX_BATCH_ROWS {
                return Err(protocol_error(rows.len(), "invalid batch row count"));
            }
            out.extend_from_slice(&(rows.len() as u16).to_le_bytes());
            for row in rows {
                out.extend_from_slice(&row.id.to_le_bytes());
                out.extend_from_slice(&row.parent_id.to_le_bytes());
                out.extend_from_slice(&row.file_record.record.to_le_bytes());
                out.extend_from_slice(&row.file_record.sequence.to_le_bytes());
                out.push(node_kind_raw(row.kind));
                out.push(0);
                out.extend_from_slice(&row.logical_bytes.to_le_bytes());
                out.extend_from_slice(&row.allocated_bytes.to_le_bytes());
                out.extend_from_slice(&row.modified_ticks.to_le_bytes());
                write_utf16(&mut out, &row.raw_name, "batch filename")?;
            }
            KIND_BATCH
        }
        DuMessage::Progress(progress) => {
            out.push(phase_raw(progress.phase));
            out.extend_from_slice(&[0; 7]);
            out.extend_from_slice(&progress.completed.to_le_bytes());
            out.extend_from_slice(&progress.total.to_le_bytes());
            out.extend_from_slice(&progress.live_records.to_le_bytes());
            out.extend_from_slice(&progress.corrupt_records.to_le_bytes());
            KIND_PROGRESS
        }
        DuMessage::Complete(complete) => {
            for value in [
                complete.rows,
                complete.logical_bytes,
                complete.allocated_bytes,
                complete.corrupt_records,
                complete.skipped_records,
                complete.start_journal_id,
            ] {
                out.extend_from_slice(&value.to_le_bytes());
            }
            out.extend_from_slice(&complete.start_next_usn.to_le_bytes());
            out.extend_from_slice(&complete.end_journal_id.to_le_bytes());
            out.extend_from_slice(&complete.end_next_usn.to_le_bytes());
            out.push(u8::from(complete.best_effort_live));
            out.extend_from_slice(&[0; 7]);
            KIND_COMPLETE
        }
        DuMessage::Failed(code) => {
            out.extend_from_slice(&failure_raw(*code).to_le_bytes());
            KIND_FAILED
        }
        DuMessage::Search(search) => {
            write_utf16(&mut out, &search.volume_guid, "volume GUID")?;
            write_utf16(&mut out, &search.root, "root path")?;
            out.extend_from_slice(&search.root_identity.volume_serial.to_le_bytes());
            out.extend_from_slice(&search.root_identity.file_id);
            out.push(u8::from(search.match_path));
            out.push(u8::from(search.include_hidden));
            out.push(u8::from(search.descend_packages));
            out.push(0);
            if search.terms.len() > MAX_SEARCH_TERMS {
                return Err(protocol_error(out.len(), "too many search terms"));
            }
            out.extend_from_slice(&(search.terms.len() as u16).to_le_bytes());
            for term in &search.terms {
                let units: Vec<u16> = term.encode_utf16().collect();
                if units.len() > MAX_SEARCH_TERM_UNITS {
                    return Err(protocol_error(out.len(), "search term too long"));
                }
                write_utf16(&mut out, &units, "search term")?;
            }
            KIND_SEARCH
        }
        DuMessage::SearchBatch(rows) => {
            if rows.is_empty() || rows.len() > MAX_BATCH_ROWS {
                return Err(protocol_error(rows.len(), "invalid search batch row count"));
            }
            out.extend_from_slice(&(rows.len() as u16).to_le_bytes());
            for row in rows {
                if !valid_relative_path(&row.relative_path) {
                    return Err(protocol_error(out.len(), "invalid search relative path"));
                }
                out.extend_from_slice(&row.file_record.record.to_le_bytes());
                out.extend_from_slice(&row.file_record.sequence.to_le_bytes());
                out.push(search_kind_raw(row.kind));
                out.push(if row.cloud { SEARCH_ROW_CLOUD } else { 0 });
                out.extend_from_slice(&row.attributes.to_le_bytes());
                out.extend_from_slice(&row.logical_bytes.to_le_bytes());
                out.extend_from_slice(&row.allocated_bytes.to_le_bytes());
                out.extend_from_slice(&row.modified_ticks.to_le_bytes());
                out.extend_from_slice(&row.created_ticks.to_le_bytes());
                write_utf16(&mut out, &row.relative_path, "search relative path")?;
            }
            KIND_SEARCH_BATCH
        }
        DuMessage::SearchComplete(complete) => {
            for value in [
                complete.matches,
                complete.entries_visited,
                complete.directories_descended,
                complete.skipped_paths,
                complete.cycles_skipped,
                complete.corrupt_records,
                complete.start_journal_id,
            ] {
                out.extend_from_slice(&value.to_le_bytes());
            }
            out.extend_from_slice(&complete.start_next_usn.to_le_bytes());
            out.extend_from_slice(&complete.end_journal_id.to_le_bytes());
            out.extend_from_slice(&complete.end_next_usn.to_le_bytes());
            out.push(u8::from(complete.index_reused));
            out.push(u8::from(complete.best_effort_live));
            out.extend_from_slice(&[0; 6]);
            KIND_SEARCH_COMPLETE
        }
        DuMessage::Release => KIND_RELEASE,
    };
    Ok((kind, out))
}

fn decode_payload(kind: u16, payload: &[u8]) -> Result<DuMessage> {
    let mut reader = FieldReader::new(payload);
    let message = match kind {
        KIND_START => {
            let volume_guid = reader.utf16("volume GUID")?;
            let root = reader.utf16("root path")?;
            let volume_serial = reader.u64()?;
            let file_id = reader.array::<16>()?;
            let sizing_mode = match reader.u8()? {
                0 => SizingMode::Apparent,
                1 => SizingMode::Allocated,
                _ => return Err(protocol_error(reader.cursor, "unknown sizing mode")),
            };
            let descend_packages = bool_field(reader.u8()?, reader.cursor)?;
            if reader.array::<2>()? != [0, 0] {
                return Err(protocol_error(
                    reader.cursor,
                    "nonzero Start reserved bytes",
                ));
            }
            let root_id = reader.u64()?;
            let first_child_id = reader.u64()?;
            DuMessage::Start(StartRequest {
                volume_guid,
                root,
                root_identity: FileIdentity {
                    volume_serial,
                    file_id,
                },
                sizing_mode,
                descend_packages,
                root_id,
                first_child_id,
            })
        }
        KIND_CANCEL => DuMessage::Cancel,
        KIND_HELLO => DuMessage::Hello {
            helper_pid: reader.u32()?,
        },
        KIND_READY => DuMessage::Ready,
        KIND_BATCH => {
            let count = usize::from(reader.u16()?);
            if count == 0 || count > MAX_BATCH_ROWS {
                return Err(protocol_error(reader.cursor, "invalid batch row count"));
            }
            let mut rows = Vec::with_capacity(count);
            for _ in 0..count {
                let id = reader.u64()?;
                let parent_id = reader.u64()?;
                let record = reader.u64()?;
                let sequence = reader.u16()?;
                let kind = node_kind(reader.u8()?, reader.cursor)?;
                if reader.u8()? != 0 {
                    return Err(protocol_error(reader.cursor, "nonzero row reserved byte"));
                }
                let logical_bytes = reader.u64()?;
                let allocated_bytes = reader.u64()?;
                let modified_ticks = reader.u64()?;
                let raw_name = reader.utf16("batch filename")?;
                if raw_name.is_empty() {
                    return Err(protocol_error(reader.cursor, "empty batch filename"));
                }
                rows.push(NeutralRow {
                    id,
                    parent_id,
                    file_record: FileReference { record, sequence },
                    kind,
                    display_name: String::from_utf16_lossy(&raw_name),
                    raw_name,
                    logical_bytes,
                    allocated_bytes,
                    modified_ticks,
                });
            }
            DuMessage::Batch(rows)
        }
        KIND_PROGRESS => {
            let phase = phase(reader.u8()?, reader.cursor)?;
            if reader.array::<7>()? != [0; 7] {
                return Err(protocol_error(
                    reader.cursor,
                    "nonzero Progress reserved bytes",
                ));
            }
            DuMessage::Progress(Progress {
                phase,
                completed: reader.u64()?,
                total: reader.u64()?,
                live_records: reader.u64()?,
                corrupt_records: reader.u64()?,
            })
        }
        KIND_COMPLETE => {
            let complete = Completion {
                rows: reader.u64()?,
                logical_bytes: reader.u64()?,
                allocated_bytes: reader.u64()?,
                corrupt_records: reader.u64()?,
                skipped_records: reader.u64()?,
                start_journal_id: reader.u64()?,
                start_next_usn: reader.i64()?,
                end_journal_id: reader.u64()?,
                end_next_usn: reader.i64()?,
                best_effort_live: bool_field(reader.u8()?, reader.cursor)?,
            };
            if reader.array::<7>()? != [0; 7] {
                return Err(protocol_error(
                    reader.cursor,
                    "nonzero Complete reserved bytes",
                ));
            }
            DuMessage::Complete(complete)
        }
        KIND_FAILED => DuMessage::Failed(failure(reader.u16()?, reader.cursor)?),
        KIND_SEARCH => {
            let volume_guid = reader.utf16("volume GUID")?;
            let root = reader.utf16("root path")?;
            let volume_serial = reader.u64()?;
            let file_id = reader.array::<16>()?;
            let match_path = bool_field(reader.u8()?, reader.cursor)?;
            let include_hidden = bool_field(reader.u8()?, reader.cursor)?;
            let descend_packages = bool_field(reader.u8()?, reader.cursor)?;
            if reader.u8()? != 0 {
                return Err(protocol_error(
                    reader.cursor,
                    "nonzero Search reserved byte",
                ));
            }
            let count = usize::from(reader.u16()?);
            if count > MAX_SEARCH_TERMS {
                return Err(protocol_error(reader.cursor, "too many search terms"));
            }
            let mut terms = Vec::with_capacity(count);
            for _ in 0..count {
                let units = reader.utf16("search term")?;
                if units.len() > MAX_SEARCH_TERM_UNITS {
                    return Err(protocol_error(reader.cursor, "search term too long"));
                }
                terms.push(
                    String::from_utf16(&units)
                        .map_err(|_| protocol_error(reader.cursor, "search term is not UTF-16"))?,
                );
            }
            DuMessage::Search(SearchRequest {
                volume_guid,
                root,
                root_identity: FileIdentity {
                    volume_serial,
                    file_id,
                },
                terms,
                match_path,
                include_hidden,
                descend_packages,
            })
        }
        KIND_SEARCH_BATCH => {
            let count = usize::from(reader.u16()?);
            if count == 0 || count > MAX_BATCH_ROWS {
                return Err(protocol_error(
                    reader.cursor,
                    "invalid search batch row count",
                ));
            }
            let mut rows = Vec::with_capacity(count);
            for _ in 0..count {
                let record = reader.u64()?;
                let sequence = reader.u16()?;
                let kind = search_kind(reader.u8()?, reader.cursor)?;
                let flags = reader.u8()?;
                if flags & !SEARCH_ROW_CLOUD != 0 {
                    return Err(protocol_error(reader.cursor, "unknown search row flags"));
                }
                let attributes = reader.u32()?;
                let logical_bytes = reader.u64()?;
                let allocated_bytes = reader.u64()?;
                let modified_ticks = reader.u64()?;
                let created_ticks = reader.u64()?;
                let relative_path = reader.utf16("search relative path")?;
                if !valid_relative_path(&relative_path) {
                    return Err(protocol_error(
                        reader.cursor,
                        "invalid search relative path",
                    ));
                }
                rows.push(SearchRow {
                    relative_path,
                    file_record: FileReference { record, sequence },
                    kind,
                    cloud: flags & SEARCH_ROW_CLOUD != 0,
                    attributes,
                    logical_bytes,
                    allocated_bytes,
                    modified_ticks,
                    created_ticks,
                });
            }
            DuMessage::SearchBatch(rows)
        }
        KIND_SEARCH_COMPLETE => {
            let complete = SearchCompletion {
                matches: reader.u64()?,
                entries_visited: reader.u64()?,
                directories_descended: reader.u64()?,
                skipped_paths: reader.u64()?,
                cycles_skipped: reader.u64()?,
                corrupt_records: reader.u64()?,
                start_journal_id: reader.u64()?,
                start_next_usn: reader.i64()?,
                end_journal_id: reader.u64()?,
                end_next_usn: reader.i64()?,
                index_reused: bool_field(reader.u8()?, reader.cursor)?,
                best_effort_live: bool_field(reader.u8()?, reader.cursor)?,
            };
            if reader.array::<6>()? != [0; 6] {
                return Err(protocol_error(
                    reader.cursor,
                    "nonzero SearchComplete reserved bytes",
                ));
            }
            DuMessage::SearchComplete(complete)
        }
        KIND_RELEASE => DuMessage::Release,
        _ => return Err(protocol_error(6, "unknown frame kind")),
    };
    if !reader.is_finished() {
        return Err(protocol_error(reader.cursor, "trailing payload bytes"));
    }
    Ok(message)
}

fn write_utf16(out: &mut Vec<u8>, units: &[u16], context: &'static str) -> Result<()> {
    if units.is_empty() || units.len() > MAX_PATH_UNITS || units.contains(&0) {
        return Err(protocol_error(out.len(), context));
    }
    out.extend_from_slice(&(units.len() as u32).to_le_bytes());
    for unit in units {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(())
}

struct FieldReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> FieldReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or_else(|| protocol_error(self.cursor, "field range overflow"))?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or_else(|| protocol_error(self.cursor, "truncated payload field"))?;
        self.cursor = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("fixed field"))
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    fn utf16(&mut self, context: &'static str) -> Result<Vec<u16>> {
        let length = usize::try_from(self.u32()?)
            .map_err(|_| protocol_error(self.cursor, "UTF-16 length does not fit usize"))?;
        if length == 0 || length > MAX_PATH_UNITS {
            return Err(protocol_error(self.cursor, context));
        }
        let byte_length = length
            .checked_mul(2)
            .ok_or_else(|| protocol_error(self.cursor, "UTF-16 byte length overflow"))?;
        let value = self.take(byte_length)?;
        let units: Vec<u16> = value
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        if units.contains(&0) {
            return Err(protocol_error(self.cursor, "UTF-16 field contains NUL"));
        }
        Ok(units)
    }

    fn is_finished(&self) -> bool {
        self.cursor == self.bytes.len()
    }
}

/// A search row path must stay below the search root once the parent joins
/// it: no empty, `.` or `..` component, no leading or trailing separator,
/// and no `/`, which Windows also treats as a separator.
pub fn valid_relative_path(units: &[u16]) -> bool {
    const DOT: u16 = b'.' as u16;
    const SLASH: u16 = b'/' as u16;
    if units.is_empty() || units.len() > MAX_PATH_UNITS || units.contains(&0) {
        return false;
    }
    if units.contains(&SLASH) {
        return false;
    }
    units
        .split(|unit| *unit == PATH_SEPARATOR)
        .all(|component| !component.is_empty() && component != [DOT] && component != [DOT, DOT])
}

fn search_kind_raw(kind: SearchEntryKind) -> u8 {
    match kind {
        SearchEntryKind::File => 0,
        SearchEntryKind::Directory => 1,
        SearchEntryKind::ReparseFile => 2,
        SearchEntryKind::ReparseDirectory => 3,
    }
}

fn search_kind(value: u8, offset: usize) -> Result<SearchEntryKind> {
    match value {
        0 => Ok(SearchEntryKind::File),
        1 => Ok(SearchEntryKind::Directory),
        2 => Ok(SearchEntryKind::ReparseFile),
        3 => Ok(SearchEntryKind::ReparseDirectory),
        _ => Err(protocol_error(offset, "unknown search entry kind")),
    }
}

fn node_kind_raw(kind: NeutralNodeKind) -> u8 {
    match kind {
        NeutralNodeKind::File => 0,
        NeutralNodeKind::Directory => 1,
        NeutralNodeKind::ReparseDirectory => 2,
        NeutralNodeKind::OpaquePackage => 3,
        NeutralNodeKind::ReparseFile => 4,
    }
}

fn node_kind(value: u8, offset: usize) -> Result<NeutralNodeKind> {
    match value {
        0 => Ok(NeutralNodeKind::File),
        1 => Ok(NeutralNodeKind::Directory),
        2 => Ok(NeutralNodeKind::ReparseDirectory),
        3 => Ok(NeutralNodeKind::OpaquePackage),
        4 => Ok(NeutralNodeKind::ReparseFile),
        _ => Err(protocol_error(offset, "unknown neutral node kind")),
    }
}

fn phase_raw(phase: ScanPhase) -> u8 {
    match phase {
        ScanPhase::Opening => 0,
        ScanPhase::MappingMft => 1,
        ScanPhase::ReadingRecords => 2,
        ScanPhase::BuildingIndex => 3,
        ScanPhase::Traversing => 4,
    }
}

fn phase(value: u8, offset: usize) -> Result<ScanPhase> {
    match value {
        0 => Ok(ScanPhase::Opening),
        1 => Ok(ScanPhase::MappingMft),
        2 => Ok(ScanPhase::ReadingRecords),
        3 => Ok(ScanPhase::BuildingIndex),
        4 => Ok(ScanPhase::Traversing),
        _ => Err(protocol_error(offset, "unknown scan phase")),
    }
}

fn failure_raw(code: FailureCode) -> u16 {
    match code {
        FailureCode::Unsupported => 1,
        FailureCode::AccessDenied => 2,
        FailureCode::Validation => 3,
        FailureCode::Protocol => 4,
        FailureCode::CorruptVolume => 5,
        FailureCode::Timeout => 6,
        FailureCode::Cancelled => 7,
        FailureCode::Internal => 8,
    }
}

fn failure(value: u16, offset: usize) -> Result<FailureCode> {
    match value {
        1 => Ok(FailureCode::Unsupported),
        2 => Ok(FailureCode::AccessDenied),
        3 => Ok(FailureCode::Validation),
        4 => Ok(FailureCode::Protocol),
        5 => Ok(FailureCode::CorruptVolume),
        6 => Ok(FailureCode::Timeout),
        7 => Ok(FailureCode::Cancelled),
        8 => Ok(FailureCode::Internal),
        _ => Err(protocol_error(offset, "unknown failure code")),
    }
}

fn bool_field(value: u8, offset: usize) -> Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(protocol_error(offset, "invalid boolean field")),
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    let field = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| protocol_error(offset, "truncated u16"))?;
    Ok(u16::from_le_bytes(field.try_into().expect("fixed field")))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    let field = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| protocol_error(offset, "truncated u32"))?;
    Ok(u32::from_le_bytes(field.try_into().expect("fixed field")))
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<u64> {
    let field = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| protocol_error(offset, "truncated u64"))?;
    Ok(u64::from_le_bytes(field.try_into().expect("fixed field")))
}

fn protocol_error(offset: usize, context: &'static str) -> NtfsError {
    NtfsError::new(ErrorKind::InvalidProtocol, offset as u64, context)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> StartRequest {
        StartRequest {
            volume_guid: r"\\?\Volume{1234}\".encode_utf16().collect(),
            root: vec![b'C' as u16, b':' as u16, b'\\' as u16, 0xd800],
            root_identity: FileIdentity {
                volume_serial: 42,
                file_id: [7; 16],
            },
            sizing_mode: SizingMode::Allocated,
            descend_packages: true,
            root_id: 100,
            first_child_id: 101,
        }
    }

    fn row() -> NeutralRow {
        let raw_name = vec![b'x' as u16, 0xd800];
        NeutralRow {
            id: 2,
            parent_id: 1,
            file_record: FileReference {
                record: 55,
                sequence: 3,
            },
            kind: NeutralNodeKind::File,
            display_name: String::from_utf16_lossy(&raw_name),
            raw_name,
            logical_bytes: 7,
            allocated_bytes: 4096,
            modified_ticks: 99,
        }
    }

    #[test]
    fn every_message_round_trips_and_preserves_raw_utf16() {
        let messages = vec![
            DuMessage::Start(request()),
            DuMessage::Cancel,
            DuMessage::Hello { helper_pid: 123 },
            DuMessage::Ready,
            DuMessage::Batch(vec![row()]),
            DuMessage::Progress(Progress {
                phase: ScanPhase::ReadingRecords,
                completed: 4,
                total: 10,
                live_records: 3,
                corrupt_records: 1,
            }),
            DuMessage::Complete(Completion {
                rows: 2,
                logical_bytes: 7,
                allocated_bytes: 4096,
                corrupt_records: 1,
                skipped_records: 2,
                start_journal_id: 3,
                start_next_usn: -4,
                end_journal_id: 5,
                end_next_usn: 6,
                best_effort_live: true,
            }),
            DuMessage::Failed(FailureCode::Validation),
        ];
        for message in messages {
            let frame = encode_frame(88, &message).unwrap();
            let (request_id, decoded) = decode_frame(&frame, Some(88)).unwrap();
            assert_eq!(request_id, 88);
            assert_eq!(decoded, message);
        }
    }

    #[test]
    fn rejects_bad_magic_version_request_lengths_and_trailing_bytes() {
        let good = encode_frame(9, &DuMessage::Ready).unwrap();
        for mutation in 0..5 {
            let mut frame = good.clone();
            match mutation {
                0 => frame[0] = b'X',
                1 => frame[4] = 3,
                2 => frame[16..20].copy_from_slice(&1u32.to_le_bytes()),
                3 => frame.push(0),
                4 => frame.truncate(10),
                _ => unreachable!(),
            }
            assert_eq!(
                decode_frame(&frame, Some(9)).unwrap_err().kind,
                ErrorKind::InvalidProtocol
            );
        }
        assert_eq!(
            decode_frame(&good, Some(10)).unwrap_err().kind,
            ErrorKind::InvalidProtocol
        );
    }

    #[test]
    fn rejects_empty_oversized_and_nul_utf16_fields() {
        for root in [Vec::new(), vec![0], vec![b'x' as u16; MAX_PATH_UNITS + 1]] {
            let mut start = request();
            start.root = root;
            assert_eq!(
                encode_frame(1, &DuMessage::Start(start)).unwrap_err().kind,
                ErrorKind::InvalidProtocol
            );
        }
    }

    fn search_request() -> SearchRequest {
        SearchRequest {
            volume_guid: r"\\?\Volume{1234}\".encode_utf16().collect(),
            root: vec![b'C' as u16, b':' as u16, b'\\' as u16, 0xdc00],
            root_identity: FileIdentity {
                volume_serial: 9,
                file_id: [3; 16],
            },
            terms: vec!["report".into(), "été".into()],
            match_path: true,
            include_hidden: false,
            descend_packages: true,
        }
    }

    fn search_row(path: &[u16]) -> SearchRow {
        SearchRow {
            relative_path: path.to_vec(),
            file_record: FileReference {
                record: 77,
                sequence: 2,
            },
            kind: SearchEntryKind::ReparseDirectory,
            cloud: true,
            attributes: 0x412,
            logical_bytes: 1,
            allocated_bytes: 2,
            modified_ticks: 3,
            created_ticks: 4,
        }
    }

    fn units(value: &str) -> Vec<u16> {
        value.encode_utf16().collect()
    }

    #[test]
    fn v2_messages_round_trip() {
        let mut path = units("Docs\\a");
        path.push(0xd800);
        let messages = vec![
            DuMessage::Search(search_request()),
            DuMessage::Search(SearchRequest {
                terms: Vec::new(),
                ..search_request()
            }),
            DuMessage::SearchBatch(vec![search_row(&path), search_row(&units("x"))]),
            DuMessage::SearchComplete(SearchCompletion {
                matches: 1,
                entries_visited: 2,
                directories_descended: 3,
                skipped_paths: 4,
                cycles_skipped: 5,
                corrupt_records: 6,
                start_journal_id: 7,
                start_next_usn: -8,
                end_journal_id: 9,
                end_next_usn: 10,
                index_reused: true,
                best_effort_live: false,
            }),
            DuMessage::Release,
        ];
        for message in messages {
            let frame = encode_frame(5, &message).unwrap();
            assert_eq!(u16_at(&frame, 4).unwrap(), PROTOCOL_VERSION);
            assert_eq!(decode_frame(&frame, Some(5)).unwrap(), (5, message));
        }
    }

    #[test]
    fn v1_disk_usage_frames_keep_version_one_bytes() {
        for message in [
            DuMessage::Start(request()),
            DuMessage::Ready,
            DuMessage::Cancel,
            DuMessage::Batch(vec![row()]),
            DuMessage::Failed(FailureCode::Cancelled),
        ] {
            let frame = encode_frame(4, &message).unwrap();
            assert_eq!(u16_at(&frame, 4).unwrap(), PROTOCOL_VERSION_V1);
            assert_eq!(decode_frame(&frame, Some(4)).unwrap().1, message);
        }
        // A v1 Batch decodes the new ReparseFile kind.
        let mut reparse = row();
        reparse.kind = NeutralNodeKind::ReparseFile;
        let frame = encode_frame(4, &DuMessage::Batch(vec![reparse.clone()])).unwrap();
        assert_eq!(
            decode_frame(&frame, Some(4)).unwrap().1,
            DuMessage::Batch(vec![reparse])
        );
    }

    #[test]
    fn v2_kinds_are_rejected_under_version_one() {
        for message in [DuMessage::Release, DuMessage::Search(search_request())] {
            let mut frame = encode_frame(1, &message).unwrap();
            frame[4..6].copy_from_slice(&PROTOCOL_VERSION_V1.to_le_bytes());
            assert_eq!(
                decode_frame(&frame, Some(1)).unwrap_err().kind,
                ErrorKind::InvalidProtocol
            );
        }
    }

    #[test]
    fn search_request_limits_are_enforced_on_both_sides() {
        let too_many = SearchRequest {
            terms: vec!["a".into(); MAX_SEARCH_TERMS + 1],
            ..search_request()
        };
        assert!(encode_frame(1, &DuMessage::Search(too_many)).is_err());
        let too_long = SearchRequest {
            terms: vec!["a".repeat(MAX_SEARCH_TERM_UNITS + 1)],
            ..search_request()
        };
        assert!(encode_frame(1, &DuMessage::Search(too_long)).is_err());
        let empty_term = SearchRequest {
            terms: vec![String::new()],
            ..search_request()
        };
        assert!(encode_frame(1, &DuMessage::Search(empty_term)).is_err());

        // Hand-patched frames: term count above the cap, an unpaired
        // surrogate in a term, a nonzero reserved byte, a bad boolean.
        let good = encode_frame(1, &DuMessage::Search(search_request())).unwrap();
        let flags_at = FRAME_HEADER_BYTES
            + 4
            + search_request().volume_guid.len() * 2
            + 4
            + search_request().root.len() * 2
            + 8
            + 16;
        let mut count = good.clone();
        count[flags_at + 4..flags_at + 6].copy_from_slice(&65u16.to_le_bytes());
        assert!(decode_frame(&count, Some(1)).is_err());
        let mut surrogate = good.clone();
        let first_term_unit = flags_at + 6 + 4;
        surrogate[first_term_unit..first_term_unit + 2].copy_from_slice(&0xd800u16.to_le_bytes());
        assert!(decode_frame(&surrogate, Some(1)).is_err());
        let mut reserved = good.clone();
        reserved[flags_at + 3] = 1;
        assert!(decode_frame(&reserved, Some(1)).is_err());
        let mut boolean = good.clone();
        boolean[flags_at] = 2;
        assert!(decode_frame(&boolean, Some(1)).is_err());
        let mut trailing = good.clone();
        trailing.push(0);
        let length = (trailing.len() - FRAME_HEADER_BYTES) as u32;
        trailing[16..20].copy_from_slice(&length.to_le_bytes());
        assert!(decode_frame(&trailing, Some(1)).is_err());
        let mut truncated = good;
        truncated.truncate(truncated.len() - 1);
        let length = (truncated.len() - FRAME_HEADER_BYTES) as u32;
        truncated[16..20].copy_from_slice(&length.to_le_bytes());
        assert!(decode_frame(&truncated, Some(1)).is_err());
    }

    #[test]
    fn search_rows_cannot_escape_the_root() {
        for path in ["..", "a\\..\\b", ".", "\\a", "a\\", "a\\\\b", "a/b", ""] {
            assert!(!valid_relative_path(&units(path)), "{path}");
            assert!(
                encode_frame(1, &DuMessage::SearchBatch(vec![search_row(&units(path))])).is_err(),
                "{path}"
            );
        }
        assert!(valid_relative_path(&units("a\\..b\\.c")));

        // A frame patched to carry ".." after encoding is rejected on decode.
        let frame =
            encode_frame(1, &DuMessage::SearchBatch(vec![search_row(&units("ab"))])).unwrap();
        let mut patched = frame.clone();
        let path_at = patched.len() - 4;
        patched[path_at..].copy_from_slice(&[b'.', 0, b'.', 0]);
        assert!(decode_frame(&patched, Some(1)).is_err());
        let mut flags = frame.clone();
        flags[FRAME_HEADER_BYTES + 2 + 8 + 2 + 1] = 0x80;
        assert!(decode_frame(&flags, Some(1)).is_err());
        let mut kind = frame;
        kind[FRAME_HEADER_BYTES + 2 + 8 + 2] = 9;
        assert!(decode_frame(&kind, Some(1)).is_err());
    }

    #[test]
    fn search_batch_is_bounded_by_rows_and_frame_bytes() {
        assert!(encode_frame(1, &DuMessage::SearchBatch(Vec::new())).is_err());
        assert!(encode_frame(
            1,
            &DuMessage::SearchBatch(vec![search_row(&units("x")); 257])
        )
        .is_err());
        let longest = vec![b'x' as u16; MAX_PATH_UNITS];
        let huge = vec![search_row(&longest); 256];
        assert_eq!(
            encode_frame(1, &DuMessage::SearchBatch(huge))
                .unwrap_err()
                .kind,
            ErrorKind::InvalidProtocol
        );
        let mut over = vec![b'x' as u16; MAX_PATH_UNITS + 1];
        over[0] = b'y' as u16;
        assert!(encode_frame(1, &DuMessage::SearchBatch(vec![search_row(&over)])).is_err());
    }

    #[test]
    fn batch_count_is_bounded() {
        assert!(encode_frame(1, &DuMessage::Batch(Vec::new())).is_err());
        assert!(encode_frame(1, &DuMessage::Batch(vec![row(); 257])).is_err());
    }
}
