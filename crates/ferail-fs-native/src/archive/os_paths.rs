// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper

//! Archive names are Unicode interchange strings. Existing OS paths remain
//! raw; only a validated individual member component crosses the encoding
//! boundary. AROS cannot represent code points above U+00FF.
use std::path::{Path, PathBuf};

/// Convert a safe archive member to its native path before creating anything.
/// Validates the whole name first, so an unrepresentable later component does
/// not leave an output file or parent directories behind.
pub fn entry_os_path(dest: &Path, entry: &str) -> std::io::Result<PathBuf> {
    let safe = ferail_archive::safe_relative_path(entry).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unsafe archive entry path",
        )
    })?;
    let mut path = dest.to_path_buf();
    for component in safe.split('/') {
        path = crate::paths::join_display_leaf(&path, component)?;
    }
    Ok(path)
}

#[cfg(unix)]
pub(super) fn component_cstring(component: &str) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    #[cfg(target_os = "aros")]
    let name = crate::paths::display_to_os(component)
        .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?;
    #[cfg(not(target_os = "aros"))]
    let name = std::ffi::OsString::from(component);
    std::ffi::CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "archive entry contains NUL",
        )
    })
}

#[cfg(all(test, target_os = "aros"))]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn unicode_interchange_name_encodes_once_and_decodes_back_without_loss() {
        let result = entry_os_path(Path::new("RAM:"), "dossier été/café.txt").unwrap();
        assert_eq!(
            result.as_os_str().as_bytes(),
            b"RAM:dossier \xe9t\xe9/caf\xe9.txt"
        );
        assert_eq!(
            crate::paths::os_to_display(result.file_name().unwrap()),
            "café.txt"
        );
        assert_eq!(
            component_cstring("café.txt").unwrap().to_bytes(),
            b"caf\xe9.txt"
        );
        assert_eq!(
            crate::paths::os_to_display(OsStr::from_bytes(b"caf\xe9.txt")),
            "café.txt"
        );
    }

    #[test]
    fn unrepresentable_names_and_unsafe_paths_fail_before_any_output() {
        for entry in [
            "dir/euro €.txt",
            "emoji 😀",
            "dir/control\n.txt",
            "../escape",
            "dir/../../escape",
            "dir/evil:name",
            "/absolute",
            "C:/escape",
        ] {
            assert!(
                entry_os_path(Path::new("RAM:unused-test-root"), entry).is_err(),
                "{entry:?}"
            );
        }
    }
    #[test]
    fn archive_creation_component_decodes_native_latin1_and_keeps_safety_rules() {
        let label = super::super::archive_component(OsStr::from_bytes(b"caf\xe9.txt"));
        assert_eq!(label, "café.txt");
        assert_eq!(
            entry_os_path(Path::new("RAM:"), &label)
                .unwrap()
                .as_os_str()
                .as_bytes(),
            b"RAM:caf\xe9.txt"
        );
        for raw in [b"..\\payload".as_slice(), b"evil:name", b"../escape"] {
            assert!(super::super::archive_component(OsStr::from_bytes(raw)).is_empty());
        }
    }
}
