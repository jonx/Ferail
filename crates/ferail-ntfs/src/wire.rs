use crate::{ErrorKind, FileReference, NeutralNodeKind, NeutralRow, NtfsError, Result};

const MAGIC: &[u8; 4] = b"FDU1";
pub const PROTOCOL_VERSION: u16 = 1;
pub const FRAME_HEADER_BYTES: usize = 20;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_PATH_UNITS: usize = 32_767;
const MAX_BATCH_ROWS: usize = 256;

const KIND_START: u16 = 1;
const KIND_CANCEL: u16 = 2;
const KIND_HELLO: u16 = 3;
const KIND_READY: u16 = 4;
const KIND_BATCH: u16 = 5;
const KIND_PROGRESS: u16 = 6;
const KIND_COMPLETE: u16 = 7;
const KIND_FAILED: u16 = 8;

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
    Hello { helper_pid: u32 },
    Ready,
    Batch(Vec<NeutralRow>),
    Progress(Progress),
    Complete(Completion),
    Failed(FailureCode),
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
    frame.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
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
    if &bytes[..4] != MAGIC || u16_at(bytes, 4)? != PROTOCOL_VERSION {
        return Err(protocol_error(0, "bad frame magic or protocol version"));
    }
    let kind = u16_at(bytes, 6)?;
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

fn node_kind_raw(kind: NeutralNodeKind) -> u8 {
    match kind {
        NeutralNodeKind::File => 0,
        NeutralNodeKind::Directory => 1,
        NeutralNodeKind::ReparseDirectory => 2,
        NeutralNodeKind::OpaquePackage => 3,
    }
}

fn node_kind(value: u8, offset: usize) -> Result<NeutralNodeKind> {
    match value {
        0 => Ok(NeutralNodeKind::File),
        1 => Ok(NeutralNodeKind::Directory),
        2 => Ok(NeutralNodeKind::ReparseDirectory),
        3 => Ok(NeutralNodeKind::OpaquePackage),
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
                1 => frame[4] = 2,
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

    #[test]
    fn batch_count_is_bounded() {
        assert!(encode_frame(1, &DuMessage::Batch(Vec::new())).is_err());
        assert!(encode_frame(1, &DuMessage::Batch(vec![row(); 257])).is_err());
    }
}
