use std::fmt;

pub type Result<T> = std::result::Result<T, NtfsError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
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
