//! Bounded, platform-neutral NTFS parsing for Ferail's optional Fast NTFS
//! Disk Usage engine.
//!
//! This crate deliberately contains no Win32, elevation, pipe or UI code.
//! Callers provide bytes through [`ByteReader`], and only Ferail-owned neutral
//! types cross the crate boundary.

#![forbid(unsafe_code)]

mod boot;
mod error;
mod reader;
mod record;
mod runlist;

pub use boot::{parse_boot_sector, NtfsGeometry};
pub use error::{ErrorKind, NtfsError, Result};
pub use reader::{ByteReader, SliceReader};
pub use record::{
    parse_file_record, AttributeList, AttributeListEntry, DataAttribute, FileName, FileRecord,
    FileReference, NameNamespace, RecordParseOptions,
};
pub use runlist::{parse_mapping_pairs, DataRun};
