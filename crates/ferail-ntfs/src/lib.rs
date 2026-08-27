//! Bounded, platform-neutral NTFS parsing for Ferail's optional Fast NTFS
//! Disk Usage engine.
//!
//! This crate deliberately contains no Win32, elevation, pipe or UI code.
//! Callers provide bytes through [`ByteReader`], and only Ferail-owned neutral
//! types cross the crate boundary.

#![forbid(unsafe_code)]

mod boot;
mod error;
mod index;
mod reader;
mod record;
mod runlist;
mod stream;
mod wire;

pub use boot::{NtfsGeometry, parse_boot_sector};
pub use error::{ErrorKind, NtfsError, Result};
pub use index::{
    CompactNtfsIndex, FileMeta, IndexBuilder, IndexStats, NameLink, NeutralNodeKind, NeutralRow,
    TraversalOptions, TraversalSummary,
};
pub use reader::{ByteReader, SliceReader};
pub use record::{
    AttributeList, AttributeListEntry, DataAttribute, FileName, FileRecord, FileReference,
    NameNamespace, RecordParseOptions, parse_attribute_list_entries, parse_file_record,
    parse_file_record_in_place,
};
pub use runlist::{DataRun, parse_mapping_pairs};
pub use stream::{MappedStream, bootstrap_mft};
pub use wire::{
    Completion, DuMessage, FRAME_HEADER_BYTES, FailureCode, FileIdentity, MAX_FRAME_BYTES,
    PROTOCOL_VERSION, Progress, ScanPhase, SizingMode, StartRequest, decode_frame, encode_frame,
};
