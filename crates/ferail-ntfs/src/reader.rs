use crate::{ErrorKind, NtfsError, Result};

/// Random-access byte source used by the parser. Implementations must either
/// fill the whole destination or return an error; short reads are never
/// accepted as implicit zeroes.
pub trait ByteReader {
    fn len(&self) -> u64;

    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> Result<()>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SliceReader<'a> {
    bytes: &'a [u8],
}

impl<'a> SliceReader<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
}

impl ByteReader for SliceReader<'_> {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> Result<()> {
        let start = usize::try_from(offset).map_err(|_| {
            NtfsError::new(
                ErrorKind::Overflow,
                offset,
                "reader offset does not fit usize",
            )
        })?;
        let end = start
            .checked_add(destination.len())
            .ok_or_else(|| NtfsError::new(ErrorKind::Overflow, offset, "reader range overflow"))?;
        let source = self.bytes.get(start..end).ok_or_else(|| {
            NtfsError::new(ErrorKind::Truncated, offset, "read extends beyond source")
        })?;
        destination.copy_from_slice(source);
        Ok(())
    }
}
