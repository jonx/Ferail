//! Small, bounded wire format for private parent → broker inputs.
//!
//! Filesystem paths must not be placed on a child process command line: the
//! Windows command line is short, globally inspectable, and cannot represent
//! a large selection. This protocol uses the child's inherited stdin and
//! preserves non-Unicode Windows paths as UTF-16.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::PathBuf;

const MAGIC: &[u8; 4] = b"FPTH";
const VERSION: u32 = 1;
const MAX_PATH_UNITS: usize = 32_767;
const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn write_paths(
    out: &mut impl Write,
    paths: &[PathBuf],
    max_count: usize,
) -> Result<(), String> {
    if paths.is_empty() || paths.len() > max_count || paths.len() > u32::MAX as usize {
        return Err("invalid private broker path count".into());
    }
    out.write_all(MAGIC).map_err(|error| error.to_string())?;
    out.write_all(&VERSION.to_le_bytes())
        .and_then(|_| out.write_all(&(paths.len() as u32).to_le_bytes()))
        .map_err(|error| error.to_string())?;

    let mut total = 0usize;
    for path in paths {
        let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if units.is_empty()
            || units.len() > MAX_PATH_UNITS
            || units.contains(&0)
            || total.saturating_add(units.len() * 2) > MAX_TOTAL_BYTES
        {
            return Err("invalid or oversized private broker path".into());
        }
        total += units.len() * 2;
        out.write_all(&(units.len() as u32).to_le_bytes())
            .map_err(|error| error.to_string())?;
        for unit in units {
            out.write_all(&unit.to_le_bytes())
                .map_err(|error| error.to_string())?;
        }
    }
    out.flush().map_err(|error| error.to_string())
}

pub(crate) fn read_paths(input: &mut impl Read, max_count: usize) -> Result<Vec<PathBuf>, String> {
    let mut magic = [0u8; 4];
    input
        .read_exact(&mut magic)
        .map_err(|error| error.to_string())?;
    if &magic != MAGIC || read_u32(input)? != VERSION {
        return Err("unsupported private broker path protocol".into());
    }
    let count = read_u32(input)? as usize;
    if count == 0 || count > max_count {
        return Err("invalid private broker path count".into());
    }

    let mut total = 0usize;
    let mut paths = Vec::with_capacity(count);
    for _ in 0..count {
        let len = read_u32(input)? as usize;
        if len == 0 || len > MAX_PATH_UNITS || total.saturating_add(len * 2) > MAX_TOTAL_BYTES {
            return Err("invalid or oversized private broker path".into());
        }
        total += len * 2;
        let mut units = Vec::with_capacity(len);
        for _ in 0..len {
            let mut bytes = [0u8; 2];
            input
                .read_exact(&mut bytes)
                .map_err(|error| error.to_string())?;
            units.push(u16::from_le_bytes(bytes));
        }
        if units.contains(&0) {
            return Err("private broker path contains NUL".into());
        }
        paths.push(PathBuf::from(OsString::from_wide(&units)));
    }
    let mut trailing = [0u8; 1];
    match input.read(&mut trailing) {
        Ok(0) => {}
        Ok(_) => return Err("trailing private broker input".into()),
        Err(error) => return Err(error.to_string()),
    }
    Ok(paths)
}

fn read_u32(input: &mut impl Read) -> Result<u32, String> {
    let mut bytes = [0u8; 4];
    input
        .read_exact(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::{read_paths, write_paths};
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;
    use std::path::PathBuf;

    #[test]
    fn paths_round_trip_without_unicode_loss() {
        let paths = vec![
            PathBuf::from(r"C:\one\ordinary.txt"),
            PathBuf::from(OsString::from_wide(&[
                b'C' as u16,
                b':' as u16,
                b'\\' as u16,
                0xD800,
                b'.' as u16,
                b't' as u16,
                b'x' as u16,
                b't' as u16,
            ])),
        ];
        let mut bytes = Vec::new();
        write_paths(&mut bytes, &paths, 8).unwrap();
        assert_eq!(read_paths(&mut bytes.as_slice(), 8).unwrap(), paths);
    }

    #[test]
    fn decode_rejects_a_count_above_the_callers_limit() {
        let paths = vec![PathBuf::from(r"C:\one\a"), PathBuf::from(r"C:\one\b")];
        let mut bytes = Vec::new();
        write_paths(&mut bytes, &paths, 8).unwrap();
        assert!(read_paths(&mut bytes.as_slice(), 1).is_err());
    }

    #[test]
    fn decode_rejects_trailing_input() {
        let paths = vec![PathBuf::from(r"C:\one\a")];
        let mut bytes = Vec::new();
        write_paths(&mut bytes, &paths, 1).unwrap();
        bytes.push(0xAA);
        assert!(read_paths(&mut bytes.as_slice(), 1).is_err());
    }
}
