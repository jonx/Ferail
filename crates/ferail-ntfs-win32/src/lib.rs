//! Windows-only raw-volume boundary for Fast NTFS.
//!
//! The public probe is unelevated and performs no raw-volume access. Opening a
//! [`RawVolumeReader`] is a separate explicit operation expected to fail with
//! access denied until the dedicated helper is elevated.

#[cfg(windows)]
mod raw;

#[cfg(windows)]
pub use raw::{probe_fast_ntfs, FastNtfsProbe, RawNtfsGeometry, RawVolumeReader};
