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

pub use boot::{parse_boot_sector, NtfsGeometry};
pub use error::{ErrorKind, NtfsError, Result};
pub use index::{
    CompactNtfsIndex, FileMeta, IndexBuilder, IndexStats, NameLink, NeutralNodeKind, NeutralRow,
    TraversalOptions, TraversalSummary,
};
pub use reader::{ByteReader, SliceReader};
pub use record::{
    parse_attribute_list_entries, parse_file_record, parse_file_record_in_place, AttributeList,
    AttributeListEntry, DataAttribute, FileName, FileRecord, FileReference, NameNamespace,
    RecordParseOptions,
};
pub use runlist::{parse_mapping_pairs, DataRun};
pub use stream::{bootstrap_mft, MappedStream};
pub use wire::{
    decode_frame, encode_frame, Completion, DuMessage, FailureCode, FileIdentity, Progress,
    ScanPhase, SizingMode, StartRequest, FRAME_HEADER_BYTES, MAX_FRAME_BYTES, PROTOCOL_VERSION,
};
