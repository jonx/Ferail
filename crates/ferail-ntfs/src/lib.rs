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
mod journal;
mod overlay;
mod reader;
mod record;
mod runlist;
mod search;
mod stream;
mod wire;

pub use boot::{parse_boot_sector, NtfsGeometry};
pub use error::{ErrorKind, NtfsError, Result};
pub use index::{
    CompactNtfsIndex, FileMeta, IndexBuilder, IndexStats, NameLink, NeutralNodeKind, NeutralRow,
    TraversalOptions, TraversalSummary, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN,
    FILE_ATTRIBUTE_REPARSE_POINT,
};
pub use journal::{
    freshness, is_indexed_record, parse_usn_read_output, unix_seconds_to_filetime, Freshness,
    FreshnessReason, JournalCursor, JournalState, ReplayCollector, ReplayPlan, UsnEntry,
    UsnReadPage, FIRST_USER_RECORD, ROOT_DIRECTORY_RECORD,
};
pub use overlay::{ApplyOutcome, IndexChange, RebuildReason};
pub use reader::{ByteReader, SliceReader};
pub use record::{
    parse_attribute_list_entries, parse_file_record, parse_file_record_in_place, AttributeList,
    AttributeListEntry, DataAttribute, FileName, FileRecord, FileReference, NameNamespace,
    RecordParseOptions,
};
pub use runlist::{parse_mapping_pairs, DataRun};
pub use search::{
    ntfs_ticks_to_unix_seconds, SearchEntryKind, SearchQuery, SearchRow, SearchSummary,
    MAX_RELATIVE_PATH_UNITS, MAX_SEARCH_TERMS, MAX_SEARCH_TERM_UNITS,
};
pub use stream::{bootstrap_mft, MappedStream};
pub use wire::{
    decode_frame, encode_frame, valid_relative_path, Completion, DuMessage, FailureCode,
    FileIdentity, Progress, ScanPhase, SearchCompletion, SearchRequest, SizingMode, StartRequest,
    FRAME_HEADER_BYTES, MAX_FRAME_BYTES, PROTOCOL_VERSION, PROTOCOL_VERSION_V1,
};
