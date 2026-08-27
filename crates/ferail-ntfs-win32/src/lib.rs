//! Windows-only raw-volume boundary for Fast NTFS.
//!
//! The public probe is unelevated and performs no raw-volume access. Opening a
//! [`RawVolumeReader`] is a separate explicit operation expected to fail with
//! access denied until the dedicated helper is elevated.

#[cfg(windows)]
mod client;
#[cfg(windows)]
mod diagnostic;
#[cfg(windows)]
mod helper;
#[cfg(windows)]
mod pipe;
#[cfg(windows)]
mod raw;
#[cfg(windows)]
mod scan;

#[cfg(windows)]
pub use client::{ClientError, FastNtfsEvent, FastNtfsRequest, run_fast_ntfs};
#[cfg(windows)]
pub use diagnostic::run_diagnostic;
#[cfg(windows)]
pub use helper::helper_main;
#[cfg(windows)]
pub use raw::{
    FastNtfsProbe, RawNtfsGeometry, RawVolumeError, RawVolumeReader, Result, file_identity,
    probe_fast_ntfs,
};
#[cfg(windows)]
pub use scan::{RawScanSummary, scan_mft};
