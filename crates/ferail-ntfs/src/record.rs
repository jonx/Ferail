use crate::{parse_mapping_pairs, DataRun, ErrorKind, NtfsError, Result};

const FILE_SIGNATURE: &[u8; 4] = b"FILE";
const ATTRIBUTE_END: u32 = 0xffff_ffff;
const ATTRIBUTE_STANDARD_INFORMATION: u32 = 0x10;
const ATTRIBUTE_LIST: u32 = 0x20;
const ATTRIBUTE_FILE_NAME: u32 = 0x30;
const ATTRIBUTE_DATA: u32 = 0x80;
const ATTRIBUTE_REPARSE_POINT: u32 = 0xc0;
const RECORD_IN_USE: u16 = 0x0001;
const RECORD_DIRECTORY: u16 = 0x0002;
const RECORD_REPARSE: u16 = 0x0400;
const ATTRIBUTE_COMPRESSED: u16 = 0x0001;
const ATTRIBUTE_SPARSE: u16 = 0x8000;
const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_ATTRIBUTES: usize = 1024;
const MAX_NAMES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileReference {
    pub record: u64,
    pub sequence: u16,
}

impl FileReference {
    pub fn from_raw(raw: u64) -> Self {
        Self {
            record: raw & 0x0000_ffff_ffff_ffff,
            sequence: (raw >> 48) as u16,
        }
    }

    pub fn is_zero(self) -> bool {
        self.record == 0 && self.sequence == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NameNamespace {
    Posix,
    Win32,
    Dos,
    Win32AndDos,
    Other(u8),
}

impl NameNamespace {
    fn from_raw(value: u8) -> Self {
        match value {
            0 => Self::Posix,
            1 => Self::Win32,
            2 => Self::Dos,
            3 => Self::Win32AndDos,
            other => Self::Other(other),
        }
    }

    pub fn is_dos_only(self) -> bool {
        self == Self::Dos
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileName {
    pub parent: FileReference,
    pub namespace: NameNamespace,
    pub name: Vec<u16>,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub file_attributes: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataAttribute {
    pub lowest_vcn: u64,
    pub highest_vcn: u64,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub initialized_bytes: u64,
    pub resident: bool,
    pub sparse: bool,
    pub compressed: bool,
    pub runs: Vec<DataRun>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttributeListEntry {
    pub attribute_type: u32,
    pub lowest_vcn: u64,
    pub record: FileReference,
    pub attribute_id: u16,
    pub name: Vec<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttributeList {
    Resident(Vec<AttributeListEntry>),
    NonResident {
        lowest_vcn: u64,
        highest_vcn: u64,
        logical_bytes: u64,
        runs: Vec<DataRun>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileRecord {
    pub record_number: u64,
    pub sequence: u16,
    pub hard_link_count: u16,
    pub in_use: bool,
    pub directory: bool,
    pub reparse_point: bool,
    pub base_reference: Option<FileReference>,
    /// Raw NTFS ticks (100 ns intervals since 1601), intentionally not
    /// converted in this parser layer.
    pub modified_ticks: Option<u64>,
    pub names: Vec<FileName>,
    pub data: Vec<DataAttribute>,
    pub attribute_lists: Vec<AttributeList>,
    pub named_data_attributes: u16,
}

impl FileRecord {
    /// Real links presented to users. A DOS 8.3 alias is suppressed when the
    /// same parent has a Win32/POSIX name, but retained when it is the only
    /// recoverable link.
    pub fn meaningful_names(&self) -> impl Iterator<Item = &FileName> {
        self.names.iter().filter(|candidate| {
            !candidate.namespace.is_dos_only()
                || !self
                    .names
                    .iter()
                    .any(|other| other.parent == candidate.parent && !other.namespace.is_dos_only())
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RecordParseOptions {
    pub bytes_per_sector: usize,
    pub expected_record_number: Option<u64>,
}

impl RecordParseOptions {
    pub const fn new(bytes_per_sector: usize) -> Self {
        Self {
            bytes_per_sector,
            expected_record_number: None,
        }
    }
}

pub fn parse_file_record(bytes: &[u8], options: RecordParseOptions) -> Result<FileRecord> {
    if bytes.len() < 48 {
        return Err(error(ErrorKind::Truncated, 0, "FILE record header"));
    }
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(error(
            ErrorKind::LimitExceeded,
            0,
            "FILE record exceeds parser limit",
        ));
    }
    if options.bytes_per_sector < 512
        || !options.bytes_per_sector.is_power_of_two()
        || bytes.len() % options.bytes_per_sector != 0
    {
        return Err(error(
            ErrorKind::InvalidGeometry,
            0,
            "record/sector size mismatch",
        ));
    }
    if &bytes[..4] != FILE_SIGNATURE {
        return Err(error(ErrorKind::InvalidSignature, 0, "FILE signature"));
    }

    let mut fixed = bytes.to_vec();
    apply_update_sequence_fixups(&mut fixed, options.bytes_per_sector)?;

    let first_attribute = usize::from(u16_at(&fixed, 20)?);
    let flags = u16_at(&fixed, 22)?;
    let used_bytes = usize::try_from(u32_at(&fixed, 24)?).map_err(|_| {
        error(
            ErrorKind::Overflow,
            24,
            "used record byte length does not fit usize",
        )
    })?;
    if first_attribute < 48 || first_attribute > used_bytes || used_bytes > fixed.len() {
        return Err(error(
            ErrorKind::InvalidAttribute,
            20,
            "attribute region outside record",
        ));
    }

    let record_number = u64::from(u32_at(&fixed, 44)?);
    if let Some(expected) = options.expected_record_number {
        if record_number != expected {
            return Err(error(
                ErrorKind::InvalidAttribute,
                44,
                "record number mismatch",
            ));
        }
    }
    let base = FileReference::from_raw(u64_at(&fixed, 32)?);
    let mut record = FileRecord {
        record_number,
        sequence: u16_at(&fixed, 16)?,
        hard_link_count: u16_at(&fixed, 18)?,
        in_use: flags & RECORD_IN_USE != 0,
        directory: flags & RECORD_DIRECTORY != 0,
        reparse_point: flags & RECORD_REPARSE != 0,
        base_reference: (!base.is_zero()).then_some(base),
        modified_ticks: None,
        names: Vec::new(),
        data: Vec::new(),
        attribute_lists: Vec::new(),
        named_data_attributes: 0,
    };

    let mut cursor = first_attribute;
    let mut attributes = 0usize;
    let mut found_end = false;
    while cursor < used_bytes {
        let attribute_type = u32_at(&fixed, cursor)?;
        if attribute_type == ATTRIBUTE_END {
            found_end = true;
            break;
        }
        attributes += 1;
        if attributes > MAX_ATTRIBUTES {
            return Err(error(
                ErrorKind::LimitExceeded,
                cursor,
                "too many attributes in one record",
            ));
        }
        let attribute_length = usize::try_from(u32_at(&fixed, cursor + 4)?).map_err(|_| {
            error(
                ErrorKind::Overflow,
                cursor + 4,
                "attribute length does not fit usize",
            )
        })?;
        if attribute_length < 24 || attribute_length % 8 != 0 {
            return Err(error(
                ErrorKind::InvalidAttribute,
                cursor + 4,
                "invalid attribute length/alignment",
            ));
        }
        let end = cursor
            .checked_add(attribute_length)
            .ok_or_else(|| error(ErrorKind::Overflow, cursor, "attribute range overflow"))?;
        if end > used_bytes {
            return Err(error(
                ErrorKind::Truncated,
                cursor,
                "attribute extends beyond used record bytes",
            ));
        }
        parse_attribute(&fixed[cursor..end], attribute_type, &mut record, cursor)?;
        cursor = end;
    }
    if !found_end {
        return Err(error(
            ErrorKind::InvalidAttribute,
            cursor,
            "missing attribute terminator",
        ));
    }
    Ok(record)
}

fn apply_update_sequence_fixups(bytes: &mut [u8], sector_size: usize) -> Result<()> {
    let usa_offset = usize::from(u16_at(bytes, 4)?);
    let usa_count = usize::from(u16_at(bytes, 6)?);
    let expected_count = bytes.len() / sector_size + 1;
    if usa_count != expected_count || usa_count < 2 {
        return Err(error(
            ErrorKind::InvalidFixup,
            6,
            "update-sequence count does not match record sectors",
        ));
    }
    let usa_bytes = usa_count.checked_mul(2).ok_or_else(|| {
        error(
            ErrorKind::Overflow,
            usa_offset,
            "update-sequence byte length overflow",
        )
    })?;
    if usa_offset < 8
        || usa_offset
            .checked_add(usa_bytes)
            .is_none_or(|end| end > bytes.len())
    {
        return Err(error(
            ErrorKind::InvalidFixup,
            usa_offset,
            "update-sequence array outside record",
        ));
    }
    let sequence = [bytes[usa_offset], bytes[usa_offset + 1]];
    for sector in 1..usa_count {
        let trailer = sector
            .checked_mul(sector_size)
            .and_then(|value| value.checked_sub(2))
            .ok_or_else(|| error(ErrorKind::Overflow, 0, "sector trailer overflow"))?;
        if bytes.get(trailer..trailer + 2) != Some(sequence.as_slice()) {
            return Err(error(
                ErrorKind::InvalidFixup,
                trailer,
                "sector trailer does not match update sequence",
            ));
        }
        let replacement = usa_offset + sector * 2;
        let original = [bytes[replacement], bytes[replacement + 1]];
        bytes[trailer..trailer + 2].copy_from_slice(&original);
    }
    Ok(())
}

fn parse_attribute(
    bytes: &[u8],
    attribute_type: u32,
    record: &mut FileRecord,
    absolute_offset: usize,
) -> Result<()> {
    let non_resident = byte_at(bytes, 8)? != 0;
    let name_length = usize::from(byte_at(bytes, 9)?);
    let name_offset = usize::from(u16_at(bytes, 10)?);
    let flags = u16_at(bytes, 12)?;
    let name = attribute_name(bytes, name_length, name_offset, absolute_offset)?;

    match (attribute_type, non_resident) {
        (ATTRIBUTE_STANDARD_INFORMATION, false) => {
            let value = resident_value(bytes, absolute_offset)?;
            if value.len() >= 16 {
                record.modified_ticks = Some(u64_at(value, 8)?);
            }
        }
        (ATTRIBUTE_FILE_NAME, false) => {
            if record.names.len() >= MAX_NAMES {
                return Err(error(
                    ErrorKind::LimitExceeded,
                    absolute_offset,
                    "too many FILE_NAME attributes",
                ));
            }
            record.names.push(parse_file_name(
                resident_value(bytes, absolute_offset)?,
                absolute_offset,
            )?);
        }
        (ATTRIBUTE_DATA, _) if !name.is_empty() => {
            record.named_data_attributes = record.named_data_attributes.saturating_add(1);
        }
        (ATTRIBUTE_DATA, false) => {
            let value = resident_value(bytes, absolute_offset)?;
            record.data.push(DataAttribute {
                lowest_vcn: 0,
                highest_vcn: 0,
                logical_bytes: value.len() as u64,
                allocated_bytes: 0,
                initialized_bytes: value.len() as u64,
                resident: true,
                sparse: false,
                compressed: false,
                runs: Vec::new(),
            });
        }
        (ATTRIBUTE_DATA, true) => {
            record
                .data
                .push(parse_non_resident(bytes, flags, absolute_offset)?);
        }
        (ATTRIBUTE_LIST, false) => {
            let value = resident_value(bytes, absolute_offset)?;
            record
                .attribute_lists
                .push(AttributeList::Resident(parse_attribute_list(
                    value,
                    absolute_offset,
                )?));
        }
        (ATTRIBUTE_LIST, true) => {
            let data = parse_non_resident(bytes, flags, absolute_offset)?;
            record.attribute_lists.push(AttributeList::NonResident {
                lowest_vcn: data.lowest_vcn,
                highest_vcn: data.highest_vcn,
                logical_bytes: data.logical_bytes,
                runs: data.runs,
            });
        }
        (ATTRIBUTE_REPARSE_POINT, _) => record.reparse_point = true,
        _ => {}
    }
    Ok(())
}

fn resident_value(bytes: &[u8], absolute_offset: usize) -> Result<&[u8]> {
    let length = usize::try_from(u32_at(bytes, 16)?).map_err(|_| {
        error(
            ErrorKind::Overflow,
            absolute_offset + 16,
            "resident value length does not fit usize",
        )
    })?;
    let offset = usize::from(u16_at(bytes, 20)?);
    range(
        bytes,
        offset,
        length,
        absolute_offset,
        "resident attribute value",
    )
}

fn parse_non_resident(bytes: &[u8], flags: u16, absolute_offset: usize) -> Result<DataAttribute> {
    if bytes.len() < 64 {
        return Err(error(
            ErrorKind::Truncated,
            absolute_offset,
            "non-resident attribute header",
        ));
    }
    let lowest_vcn = u64_at(bytes, 16)?;
    let highest_vcn = u64_at(bytes, 24)?;
    let mapping_offset = usize::from(u16_at(bytes, 32)?);
    if mapping_offset < 64 || mapping_offset >= bytes.len() {
        return Err(error(
            ErrorKind::InvalidAttribute,
            absolute_offset + 32,
            "mapping pairs outside attribute",
        ));
    }
    let runs = parse_mapping_pairs(&bytes[mapping_offset..], lowest_vcn).map_err(|mut err| {
        err.offset = err
            .offset
            .saturating_add((absolute_offset + mapping_offset) as u64);
        err
    })?;
    if let Some(last) = runs.last() {
        let last_vcn = last
            .vcn
            .checked_add(last.cluster_count)
            .and_then(|value| value.checked_sub(1))
            .ok_or_else(|| {
                error(
                    ErrorKind::Overflow,
                    absolute_offset + mapping_offset,
                    "run VCN range overflow",
                )
            })?;
        if last_vcn != highest_vcn {
            return Err(error(
                ErrorKind::InvalidRunlist,
                absolute_offset + 24,
                "highest VCN does not match mapping pairs",
            ));
        }
    }
    Ok(DataAttribute {
        lowest_vcn,
        highest_vcn,
        allocated_bytes: u64_at(bytes, 40)?,
        logical_bytes: u64_at(bytes, 48)?,
        initialized_bytes: u64_at(bytes, 56)?,
        resident: false,
        sparse: flags & ATTRIBUTE_SPARSE != 0 || runs.iter().any(|run| run.lcn.is_none()),
        compressed: flags & ATTRIBUTE_COMPRESSED != 0,
        runs,
    })
}

fn parse_file_name(bytes: &[u8], absolute_offset: usize) -> Result<FileName> {
    if bytes.len() < 66 {
        return Err(error(
            ErrorKind::Truncated,
            absolute_offset,
            "FILE_NAME value",
        ));
    }
    let length = usize::from(bytes[64]);
    let name_bytes = length.checked_mul(2).ok_or_else(|| {
        error(
            ErrorKind::Overflow,
            absolute_offset + 64,
            "FILE_NAME UTF-16 length overflow",
        )
    })?;
    let encoded = range(bytes, 66, name_bytes, absolute_offset, "FILE_NAME UTF-16")?;
    let name = encoded
        .chunks_exact(2)
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        .collect();
    Ok(FileName {
        parent: FileReference::from_raw(u64_at(bytes, 0)?),
        allocated_bytes: u64_at(bytes, 40)?,
        logical_bytes: u64_at(bytes, 48)?,
        file_attributes: u32_at(bytes, 56)?,
        namespace: NameNamespace::from_raw(bytes[65]),
        name,
    })
}

fn parse_attribute_list(bytes: &[u8], absolute_offset: usize) -> Result<Vec<AttributeListEntry>> {
    let mut entries = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor..].iter().all(|byte| *byte == 0) {
            break;
        }
        if bytes.len() - cursor < 26 {
            return Err(error(
                ErrorKind::Truncated,
                absolute_offset + cursor,
                "ATTRIBUTE_LIST entry header",
            ));
        }
        let attribute_type = u32_at(bytes, cursor)?;
        if attribute_type == ATTRIBUTE_END {
            break;
        }
        let length = usize::from(u16_at(bytes, cursor + 4)?);
        if length < 26
            || cursor
                .checked_add(length)
                .is_none_or(|end| end > bytes.len())
        {
            return Err(error(
                ErrorKind::InvalidAttribute,
                absolute_offset + cursor + 4,
                "ATTRIBUTE_LIST entry length",
            ));
        }
        let entry = &bytes[cursor..cursor + length];
        let name_length = usize::from(entry[6]);
        let name_offset = usize::from(entry[7]);
        let name = attribute_name(entry, name_length, name_offset, absolute_offset + cursor)?;
        entries.push(AttributeListEntry {
            attribute_type,
            lowest_vcn: u64_at(entry, 8)?,
            record: FileReference::from_raw(u64_at(entry, 16)?),
            attribute_id: u16_at(entry, 24)?,
            name,
        });
        cursor += length;
    }
    Ok(entries)
}

/// Parses the logical contents of a non-resident `$ATTRIBUTE_LIST` stream.
/// The caller remains responsible for mapping its runs and enforcing a byte
/// cap before materializing the small metadata stream.
pub fn parse_attribute_list_entries(bytes: &[u8]) -> Result<Vec<AttributeListEntry>> {
    parse_attribute_list(bytes, 0)
}

fn attribute_name(
    bytes: &[u8],
    length: usize,
    offset: usize,
    absolute_offset: usize,
) -> Result<Vec<u16>> {
    if length == 0 {
        return Ok(Vec::new());
    }
    let byte_length = length.checked_mul(2).ok_or_else(|| {
        error(
            ErrorKind::Overflow,
            absolute_offset + 9,
            "attribute name length overflow",
        )
    })?;
    let encoded = range(
        bytes,
        offset,
        byte_length,
        absolute_offset,
        "attribute name",
    )?;
    Ok(encoded
        .chunks_exact(2)
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        .collect())
}

fn range<'a>(
    bytes: &'a [u8],
    offset: usize,
    length: usize,
    absolute_offset: usize,
    context: &'static str,
) -> Result<&'a [u8]> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| error(ErrorKind::Overflow, absolute_offset + offset, context))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| error(ErrorKind::Truncated, absolute_offset + offset, context))
}

fn byte_at(bytes: &[u8], offset: usize) -> Result<u8> {
    bytes
        .get(offset)
        .copied()
        .ok_or_else(|| error(ErrorKind::Truncated, offset, "u8 field"))
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = range(bytes, offset, 2, 0, "u16 field")?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = range(bytes, offset, 4, 0, "u32 field")?;
    Ok(u32::from_le_bytes(value.try_into().expect("fixed field")))
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<u64> {
    let value = range(bytes, offset, 8, 0, "u64 field")?;
    Ok(u64::from_le_bytes(value.try_into().expect("fixed field")))
}

fn error(kind: ErrorKind, offset: usize, context: &'static str) -> NtfsError {
    NtfsError::new(kind, offset as u64, context)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD_SIZE: usize = 1024;
    const SECTOR_SIZE: usize = 512;

    struct RecordFixture {
        bytes: Vec<u8>,
        cursor: usize,
    }

    impl RecordFixture {
        fn new(record_number: u32, flags: u16) -> Self {
            let mut bytes = vec![0u8; RECORD_SIZE];
            bytes[..4].copy_from_slice(b"FILE");
            put_u16(&mut bytes, 4, 48);
            put_u16(&mut bytes, 6, 3);
            put_u16(&mut bytes, 16, 7);
            put_u16(&mut bytes, 18, 1);
            put_u16(&mut bytes, 20, 56);
            put_u16(&mut bytes, 22, flags);
            put_u32(&mut bytes, 44, record_number);
            Self { bytes, cursor: 56 }
        }

        fn resident(&mut self, kind: u32, value: &[u8], attribute_id: u16) {
            let length = align8(24 + value.len());
            put_u32(&mut self.bytes, self.cursor, kind);
            put_u32(&mut self.bytes, self.cursor + 4, length as u32);
            put_u16(&mut self.bytes, self.cursor + 14, attribute_id);
            put_u32(&mut self.bytes, self.cursor + 16, value.len() as u32);
            put_u16(&mut self.bytes, self.cursor + 20, 24);
            self.bytes[self.cursor + 24..self.cursor + 24 + value.len()].copy_from_slice(value);
            self.cursor += length;
        }

        fn non_resident_data(&mut self, pairs: &[u8], highest_vcn: u64, flags: u16) {
            let length = align8(64 + pairs.len());
            put_u32(&mut self.bytes, self.cursor, ATTRIBUTE_DATA);
            put_u32(&mut self.bytes, self.cursor + 4, length as u32);
            self.bytes[self.cursor + 8] = 1;
            put_u16(&mut self.bytes, self.cursor + 12, flags);
            put_u64(&mut self.bytes, self.cursor + 16, 0);
            put_u64(&mut self.bytes, self.cursor + 24, highest_vcn);
            put_u16(&mut self.bytes, self.cursor + 32, 64);
            put_u64(&mut self.bytes, self.cursor + 40, 24_576);
            put_u64(&mut self.bytes, self.cursor + 48, 20_000);
            put_u64(&mut self.bytes, self.cursor + 56, 20_000);
            self.bytes[self.cursor + 64..self.cursor + 64 + pairs.len()].copy_from_slice(pairs);
            self.cursor += length;
        }

        fn finish(mut self) -> Vec<u8> {
            put_u32(&mut self.bytes, self.cursor, ATTRIBUTE_END);
            self.cursor += 8;
            put_u32(&mut self.bytes, 24, self.cursor as u32);
            put_u32(&mut self.bytes, 28, RECORD_SIZE as u32);

            // Three-entry USA: sequence plus one original trailer per sector.
            put_u16(&mut self.bytes, 48, 0xa55a);
            put_u16(&mut self.bytes, 50, 0x1111);
            put_u16(&mut self.bytes, 52, 0x2222);
            put_u16(&mut self.bytes, SECTOR_SIZE - 2, 0xa55a);
            put_u16(&mut self.bytes, RECORD_SIZE - 2, 0xa55a);
            self.bytes
        }
    }

    fn file_name(parent: u64, namespace: u8, name: &[u16]) -> Vec<u8> {
        let mut value = vec![0u8; 66 + name.len() * 2];
        put_u64(&mut value, 0, parent);
        put_u64(&mut value, 40, 8192);
        put_u64(&mut value, 48, 5000);
        put_u32(&mut value, 56, 0x20);
        value[64] = name.len() as u8;
        value[65] = namespace;
        for (index, unit) in name.iter().enumerate() {
            put_u16(&mut value, 66 + index * 2, *unit);
        }
        value
    }

    #[test]
    fn valid_fixture_applies_fixups_and_preserves_utf16_names() {
        let mut fixture = RecordFixture::new(42, RECORD_IN_USE);
        let mut standard = vec![0u8; 48];
        put_u64(&mut standard, 8, 123_456);
        fixture.resident(ATTRIBUTE_STANDARD_INFORMATION, &standard, 1);
        let raw_name = [b'a' as u16, 0xd800, b'z' as u16];
        fixture.resident(ATTRIBUTE_FILE_NAME, &file_name(5, 1, &raw_name), 2);
        fixture.resident(ATTRIBUTE_DATA, b"resident payload", 3);

        let record =
            parse_file_record(&fixture.finish(), RecordParseOptions::new(SECTOR_SIZE)).unwrap();
        assert_eq!(record.record_number, 42);
        assert_eq!(record.modified_ticks, Some(123_456));
        assert_eq!(record.names[0].name, raw_name);
        assert_eq!(record.data[0].logical_bytes, 16);
    }

    #[test]
    fn bad_fixup_and_truncated_attribute_fixtures_are_rejected() {
        let mut bad_fixup = RecordFixture::new(1, RECORD_IN_USE).finish();
        put_u16(&mut bad_fixup, SECTOR_SIZE - 2, 0xbeef);
        assert_eq!(
            parse_file_record(&bad_fixup, RecordParseOptions::new(SECTOR_SIZE))
                .unwrap_err()
                .kind,
            ErrorKind::InvalidFixup
        );

        let mut truncated = RecordFixture::new(1, RECORD_IN_USE).finish();
        put_u32(&mut truncated, 56, ATTRIBUTE_DATA);
        put_u32(&mut truncated, 60, 904);
        assert_eq!(
            parse_file_record(&truncated, RecordParseOptions::new(SECTOR_SIZE))
                .unwrap_err()
                .kind,
            ErrorKind::Truncated
        );
    }

    #[test]
    fn parses_fragmented_sparse_nonresident_data() {
        let mut fixture = RecordFixture::new(9, RECORD_IN_USE);
        fixture.non_resident_data(&[0x11, 3, 100, 0x01, 2, 0x11, 1, 4, 0], 5, ATTRIBUTE_SPARSE);
        let record =
            parse_file_record(&fixture.finish(), RecordParseOptions::new(SECTOR_SIZE)).unwrap();
        assert_eq!(record.data[0].runs.len(), 3);
        assert!(record.data[0].sparse);
        assert_eq!(record.data[0].runs[1].lcn, None);
    }

    #[test]
    fn parses_attribute_list_extension_reference() {
        let mut list = vec![0u8; 32];
        put_u32(&mut list, 0, ATTRIBUTE_DATA);
        put_u16(&mut list, 4, 32);
        put_u64(&mut list, 8, 4);
        put_u64(&mut list, 16, (11u64 << 48) | 77);
        put_u16(&mut list, 24, 5);
        let mut fixture = RecordFixture::new(12, RECORD_IN_USE);
        fixture.resident(ATTRIBUTE_LIST, &list, 1);
        let record =
            parse_file_record(&fixture.finish(), RecordParseOptions::new(SECTOR_SIZE)).unwrap();
        let AttributeList::Resident(entries) = &record.attribute_lists[0] else {
            panic!("expected resident attribute list")
        };
        assert_eq!(entries[0].record.record, 77);
        assert_eq!(entries[0].record.sequence, 11);
        assert_eq!(entries[0].lowest_vcn, 4);
    }

    #[test]
    fn dos_alias_is_hidden_when_real_name_exists_for_same_parent() {
        let mut fixture = RecordFixture::new(15, RECORD_IN_USE);
        fixture.resident(
            ATTRIBUTE_FILE_NAME,
            &file_name(5, 2, &"LONGFI~1.TXT".encode_utf16().collect::<Vec<_>>()),
            1,
        );
        fixture.resident(
            ATTRIBUTE_FILE_NAME,
            &file_name(
                5,
                1,
                &"Long filename.txt".encode_utf16().collect::<Vec<_>>(),
            ),
            2,
        );
        let record =
            parse_file_record(&fixture.finish(), RecordParseOptions::new(SECTOR_SIZE)).unwrap();
        assert_eq!(record.meaningful_names().count(), 1);
        assert_eq!(
            String::from_utf16_lossy(&record.meaningful_names().next().unwrap().name),
            "Long filename.txt"
        );
    }

    fn align8(value: usize) -> usize {
        (value + 7) & !7
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}
