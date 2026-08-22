//! Path-traversal ("zip slip") validation for archive entries.
//!
//! An archive is untrusted input: an entry named `../../etc/passwd` or
//! `/etc/passwd` would, if joined naively to the destination, write *outside*
//! it. Going pure-Rust on macOS means we no longer get `ditto`'s built-in
//! containment for free, so every extractor MUST route each entry path through
//! [`safe_relative_path`] before creating a file. This is pure string logic —
//! the codec layer still owns the on-disk symlink checks it can only do with
//! real paths, but the traversal decision lives here so it is unit-testable and
//! shared by every format.

/// Why an entry path was rejected as unsafe to extract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsafePath {
    /// The path is absolute (`/etc/passwd`) — it would escape the destination.
    Absolute,
    /// The path contains a `..` component that escapes the destination root.
    Traversal,
    /// The path carries a Windows drive prefix or UNC root (`C:\`, `\\host`).
    DrivePrefix,
    /// The path is empty or resolves to nothing.
    Empty,
    /// A component contains a byte the host filesystem cannot represent.
    InvalidCharacter,
}

/// Validate and normalize an archive entry path into a safe, destination-
/// relative path with `/` separators.
///
/// On success the returned string is guaranteed to:
/// - be relative (never starts with `/`),
/// - contain no `..` or `.` components,
/// - stay within the destination when joined to it.
///
/// Callers join the result to the extraction destination and create the file
/// there. Rejects anything that would escape.
pub fn safe_relative_path(entry_path: &str) -> Result<String, UnsafePath> {
    // Normalize separators; archives may use either, and a Windows-authored
    // zip can carry backslashes.
    let unified = entry_path.replace('\\', "/");

    // Reject Windows drive / UNC roots (`C:/...`, `//host/share`).
    if unified.starts_with("//") {
        return Err(UnsafePath::DrivePrefix);
    }
    if let Some((head, _)) = unified.split_once('/') {
        if is_drive_component(head) {
            return Err(UnsafePath::DrivePrefix);
        }
    } else if is_drive_component(&unified) {
        return Err(UnsafePath::DrivePrefix);
    }

    // Reject POSIX-absolute paths.
    if unified.starts_with('/') {
        return Err(UnsafePath::Absolute);
    }

    // Walk components, folding `.` away and rejecting any `..` (we do not
    // allow *any* traversal, even one that would stay in-bounds after the
    // fact — the simplest safe rule).
    let mut parts: Vec<&str> = Vec::new();
    for comp in unified.split('/') {
        match comp {
            "" | "." => continue,
            ".." => return Err(UnsafePath::Traversal),
            other if other.contains('\0') => return Err(UnsafePath::InvalidCharacter),
            other => parts.push(other),
        }
    }

    if parts.is_empty() {
        return Err(UnsafePath::Empty);
    }
    Ok(parts.join("/"))
}

/// Whether a path component looks like a Windows drive spec (`C:`), which must
/// never be treated as a directory name.
fn is_drive_component(comp: &str) -> bool {
    let bytes = comp.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}
