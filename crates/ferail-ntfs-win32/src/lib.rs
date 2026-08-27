//! Windows-only raw-volume boundary for Fast NTFS.
//!
//! The public probe is unelevated and performs no raw-volume access. Opening a
//! [`RawVolumeReader`] is a separate explicit operation expected to fail with
//! access denied until the dedicated helper is elevated.

#[cfg(windows)]
mod client;
#[cfg(windows)]
mod helper;
#[cfg(windows)]
mod pipe;
#[cfg(windows)]
mod raw;
#[cfg(windows)]
mod scan;

#[cfg(windows)]
pub use client::{run_fast_ntfs, ClientError, FastNtfsEvent, FastNtfsRequest};
#[cfg(windows)]
pub use helper::helper_main;
#[cfg(windows)]
pub use raw::{
    file_identity, probe_fast_ntfs, FastNtfsProbe, RawNtfsGeometry, RawVolumeError,
    RawVolumeReader, Result,
};
#[cfg(windows)]
pub use scan::{scan_mft, RawScanSummary};
