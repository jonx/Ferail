use std::fmt;

pub type Result<T> = std::result::Result<T, NtfsError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Cancelled,
    SourceIo,
    Truncated,
    InvalidSignature,
    InvalidGeometry,
    InvalidFixup,
    InvalidAttribute,
    InvalidRunlist,
    Overflow,
    LimitExceeded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NtfsError {
    pub kind: ErrorKind,
    pub offset: u64,
    pub context: &'static str,
}

impl NtfsError {
    pub(crate) const fn new(kind: ErrorKind, offset: u64, context: &'static str) -> Self {
        Self {
            kind,
            offset,
            context,
        }
    }

    /// Builds a source-reader error without carrying an OS message into the
    /// neutral parser or its logs.
    pub const fn source(offset: u64, context: &'static str) -> Self {
        Self::new(ErrorKind::SourceIo, offset, context)
    }

    pub const fn cancelled(context: &'static str) -> Self {
        Self::new(ErrorKind::Cancelled, 0, context)
    }
}

impl fmt::Display for NtfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "NTFS {:?} at byte {} ({})",
            self.kind, self.offset, self.context
        )
    }
}

impl std::error::Error for NtfsError {}
