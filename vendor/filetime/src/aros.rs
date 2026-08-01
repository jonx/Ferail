//! AROS arm (vendored addition): reads via std `Metadata` (the rust-aros
//! std implements modified/accessed over posixc stat), writes unsupported —
//! nothing in the gpui/Feraille graph sets file times on AROS (notify's
//! poll watcher only *reads* mtimes).

use crate::FileTime;
use std::fs;
use std::io;
use std::path::Path;
use std::time::UNIX_EPOCH;

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "setting file times is not supported on AROS",
    )
}

fn from_system_time(time: std::time::SystemTime) -> FileTime {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => FileTime {
            seconds: d.as_secs() as i64,
            nanos: d.subsec_nanos(),
        },
        Err(e) => {
            let d = e.duration();
            FileTime {
                seconds: -(d.as_secs() as i64),
                nanos: d.subsec_nanos(),
            }
        }
    }
}

pub fn set_symlink_file_times(_p: &Path, _atime: FileTime, _mtime: FileTime) -> io::Result<()> {
    Err(unsupported())
}

pub fn from_last_modification_time(meta: &fs::Metadata) -> FileTime {
    meta.modified()
        .map(from_system_time)
        .unwrap_or(FileTime::zero())
}

pub fn from_last_access_time(meta: &fs::Metadata) -> FileTime {
    meta.accessed()
        .map(from_system_time)
        .unwrap_or(FileTime::zero())
}

pub fn from_creation_time(meta: &fs::Metadata) -> Option<FileTime> {
    meta.created().ok().map(from_system_time)
}

pub fn open(path: &Path) -> io::Result<fs::File> {
    fs::File::open(path)
}
