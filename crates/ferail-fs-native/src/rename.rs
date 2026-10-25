// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper

//! Renaming user items without replacing an existing destination.
//!
//! This operation must run on a filesystem worker. AROS uses DOS Rename
//! (ACTION_RENAME_OBJECT), bypassing C rename's destination deletion. It
//! supports files and directories on the same volume, including case-only
//! renames when the native handler recognizes the same object. A collision
//! returns AlreadyExists and preserves both items. No exists-then-rename race.

use std::path::Path;

/// Rename without overwriting another item. Unsupported platforms return
/// an explicit error; they never fall back to destructive standard rename.
pub fn rename_no_replace(source: &Path, target: &Path) -> std::io::Result<()> {
    ferail_core::path_guard::assert_off_ui_thread("rename::rename_no_replace");
    rename_native(source, target)
}

/// Construct a child in AROS syntax, preserving a volume-root colon.
/// Pure byte manipulation; safe on the foreground thread.
#[cfg(any(target_os = "aros", all(test, unix)))]
pub fn child_native_path(
    parent: &Path,
    leaf: &std::ffi::OsStr,
) -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    validate_native_leaf(leaf.as_bytes())?;
    Ok(join_native_parent(
        parent.as_os_str().as_bytes(),
        leaf.as_bytes(),
    ))
}

/// Construct a sibling in AROS path syntax without treating `RAM:old` as
/// one Unix filename. Pure byte manipulation; safe on the foreground thread.
#[cfg(any(target_os = "aros", all(test, unix)))]
pub fn sibling_native_path(
    source: &Path,
    leaf: &std::ffi::OsStr,
) -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let (parent, _) = split_native_destination(source.as_os_str().as_bytes())?;
    validate_native_leaf(leaf.as_bytes())?;
    Ok(join_native_parent(parent.unwrap_or(b""), leaf.as_bytes()))
}

#[cfg(any(target_os = "aros", all(test, unix)))]
fn validate_native_leaf(leaf: &[u8]) -> std::io::Result<()> {
    if leaf.is_empty()
        || leaf == b"."
        || leaf == b".."
        || leaf.iter().any(|b| matches!(*b, b':' | b'/' | 0))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a rename destination must have a filename without colon, slash or NUL",
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "aros", all(test, unix)))]
fn join_native_parent(parent: &[u8], leaf: &[u8]) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = parent.to_vec();
    if !bytes.is_empty() && !bytes.ends_with(b":") && !bytes.ends_with(b"/") {
        bytes.push(b'/');
    }
    bytes.extend_from_slice(leaf);
    std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&bytes))
}

// Kept byte based: AROS names need not be UTF-8, and a colon is a
// volume/assign delimiter only before any slash. Never let a target leaf
// introduce another assign or an empty/current/parent directory component.
#[cfg(any(target_os = "aros", all(test, unix)))]
fn split_native_destination(bytes: &[u8]) -> std::io::Result<(Option<&[u8]>, &[u8])> {
    let (parent, leaf) = if let Some(slash) = bytes.iter().rposition(|b| *b == b'/') {
        (Some(&bytes[..slash]), &bytes[slash + 1..])
    } else if let Some(colon) = bytes.iter().position(|b| *b == b':') {
        (Some(&bytes[..=colon]), &bytes[colon + 1..])
    } else {
        (None, bytes)
    };
    validate_native_leaf(leaf)?;
    if bytes.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rename path contains NUL",
        ));
    }
    Ok((parent, leaf))
}

// Resolve only the parent. Resolving the complete source would follow a
// final soft link and rename its referent, and would reject dangling links.
#[cfg(any(target_os = "aros", all(test, unix)))]
fn resolve_native_entry(
    path: &Path,
    resolve_parent: &mut impl FnMut(Option<&[u8]>) -> std::io::Result<std::path::PathBuf>,
) -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let (parent, leaf) = split_native_destination(path.as_os_str().as_bytes())?;
    let parent = resolve_parent(parent)?;
    Ok(join_native_parent(parent.as_os_str().as_bytes(), leaf))
}

// Share the exact AROS path preparation with captured-transport tests; the
// transport receives the original final leaves, even for dangling soft links.
#[cfg(any(target_os = "aros", all(test, unix)))]
fn rename_aros_with(
    source: &Path,
    target: &Path,
    mut resolve_parent: impl FnMut(Option<&[u8]>) -> std::io::Result<std::path::PathBuf>,
    transport: impl FnOnce(&std::ffi::CStr, &std::ffi::CStr) -> i32,
) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let source = resolve_native_entry(source, &mut resolve_parent)?;
    let target = resolve_native_entry(target, &mut resolve_parent)?;
    let native = |path: &Path| -> std::io::Result<CString> {
        let bytes = path.as_os_str().as_bytes();
        if bytes.first() == Some(&b'/') || !bytes.contains(&b':') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "rename requires native AROS volume paths",
            ));
        }
        CString::new(bytes).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "rename path contains NUL")
        })
    };
    let source = native(&source)?;
    let target = native(&target)?;
    let error = transport(&source, &target);
    if error == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(error))
    }
}

#[cfg(target_os = "aros")]
fn rename_native(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn fsn_aros_rename_noreplace(
            source: *const std::ffi::c_char,
            target: *const std::ffi::c_char,
        ) -> i32;
    }
    rename_aros_with(
        source,
        target,
        |parent| {
            // Native realpath uses Lock/NameFromLock; final entry is deliberately
            // excluded from this lookup, so no source-link dereference occurs.
            match parent {
                Some(bytes) => std::fs::canonicalize(Path::new(OsStr::from_bytes(bytes))),
                None => std::env::current_dir(),
            }
        },
        |source, target| {
            // Owned C strings remain alive through this synchronous DOS call.
            unsafe { fsn_aros_rename_noreplace(source.as_ptr(), target.as_ptr()) }
        },
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rename_native(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let encode = |path: &Path| {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "rename path contains NUL")
        })
    };
    let source = encode(source)?;
    let target = encode(target)?;
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "aros", target_os = "macos", target_os = "linux")))]
fn rename_native(_source: &Path, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic rename without replacement is not implemented on this platform",
    ))
}

#[cfg(all(
    test,
    any(target_os = "aros", target_os = "macos", target_os = "linux")
))]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "ferail-rename-contract-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn collision_and_reoccupied_undo_preserve_both_files() {
        let fixture = Fixture::new();
        let original = fixture.0.join("original é.txt");
        let renamed = fixture.0.join("existing file.txt");
        fs::write(&original, b"source bytes").unwrap();
        fs::write(&renamed, b"destination bytes").unwrap();
        assert_eq!(
            rename_no_replace(&original, &renamed).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&original).unwrap(), b"source bytes");
        assert_eq!(fs::read(&renamed).unwrap(), b"destination bytes");
        fs::remove_file(&renamed).unwrap();
        rename_no_replace(&original, &renamed).unwrap();
        assert!(!original.exists());
        assert_eq!(fs::read(&renamed).unwrap(), b"source bytes");
        // Reoccupation between rename and Undo must not destroy new data.
        fs::write(&original, b"new occupant").unwrap();
        assert_eq!(
            rename_no_replace(&renamed, &original).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&original).unwrap(), b"new occupant");
        assert_eq!(fs::read(&renamed).unwrap(), b"source bytes");
    }

    #[test]
    fn directories_collide_without_merging_and_can_be_renamed() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source folder");
        let target = fixture.0.join("target folder");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(source.join("source.txt"), b"source").unwrap();
        fs::write(target.join("target.txt"), b"target").unwrap();
        assert_eq!(
            rename_no_replace(&source, &target).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(source.join("source.txt")).unwrap(), b"source");
        assert_eq!(fs::read(target.join("target.txt")).unwrap(), b"target");
        let unused = fixture.0.join("new folder é");
        rename_no_replace(&source, &unused).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read(unused.join("source.txt")).unwrap(), b"source");
    }

    #[test]
    fn raw_names_and_errors_do_not_change_the_source() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let fixture = Fixture::new();
        let source = fixture
            .0
            .join(OsString::from_vec(if cfg!(target_os = "macos") {
                "raw-é-source".as_bytes().to_vec()
            } else {
                b"raw-\xe9-source".to_vec()
            }));
        let target = fixture
            .0
            .join(OsString::from_vec(if cfg!(target_os = "macos") {
                "raw-é-target".as_bytes().to_vec()
            } else {
                b"raw-\xff-target".to_vec()
            }));
        fs::write(&source, b"raw filename content").unwrap();
        rename_no_replace(&source, &target).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read(&target).unwrap(), b"raw filename content");
        let bad = fixture.0.join(OsString::from_vec(b"bad\0name".to_vec()));
        assert_eq!(
            rename_no_replace(&target, &bad).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(fs::read(&target).unwrap(), b"raw filename content");
        assert_eq!(
            rename_no_replace(&source, &target).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }
    #[test]
    fn native_destination_parser_preserves_volume_root_and_raw_bytes() {
        for (path, parent, leaf) in [
            (
                b"RAM:file".as_slice(),
                Some(b"RAM:".as_slice()),
                b"file".as_slice(),
            ),
            (
                b"SYS:folder/leaf",
                Some(b"SYS:folder".as_slice()),
                b"leaf".as_slice(),
            ),
            (b"relative leaf", None, b"relative leaf".as_slice()),
            (
                b"WORK:raw-\xe9/\xff",
                Some(b"WORK:raw-\xe9".as_slice()),
                b"\xff".as_slice(),
            ),
        ] {
            assert_eq!(split_native_destination(path).unwrap(), (parent, leaf));
        }
        for bad in [
            b"RAM:folder/evil:name".as_slice(),
            b"RAM:bad:leaf",
            b"RAM:",
            b"RAM:folder/",
            b"RAM:folder/..",
            b"RAM:bad\0leaf",
        ] {
            assert_eq!(
                split_native_destination(bad).unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput
            );
        }
    }
    #[test]
    fn sibling_paths_preserve_volume_root_directories_and_raw_names() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        for (source, leaf, expected) in [
            (
                b"RAM:old".as_slice(),
                b"new".as_slice(),
                b"RAM:new".as_slice(),
            ),
            (b"WORK:dir/old", b"new", b"WORK:dir/new"),
            (b"WORK:raw-\xe9/old", b"new-\xff", b"WORK:raw-\xe9/new-\xff"),
            (b"old", b"new", b"new"),
        ] {
            let result = sibling_native_path(
                Path::new(OsStr::from_bytes(source)),
                OsStr::from_bytes(leaf),
            )
            .unwrap();
            assert_eq!(result.as_os_str().as_bytes(), expected);
        }
        for leaf in [
            b"evil:name".as_slice(),
            b"bad/name",
            b"bad\0name",
            b"..",
            b"",
        ] {
            assert_eq!(
                sibling_native_path(Path::new("RAM:old"), OsStr::from_bytes(leaf))
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidInput
            );
        }
    }
    #[test]
    fn aros_transport_resolves_only_parents_and_keeps_final_link_bytes() {
        use std::os::unix::ffi::OsStrExt;
        for leaf in [b"valid-link".as_slice(), b"dangling-\xe9-link".as_slice()] {
            let source = join_native_parent(b"ASSIGN:dir", leaf);
            let target = Path::new("RAM:renamed");
            let mut requested_parents = Vec::new();
            rename_aros_with(
                &source,
                target,
                |parent| {
                    let bytes = parent.expect("absolute native parent");
                    requested_parents.push(bytes.to_vec());
                    // This is the only filesystem lookup boundary. A full source
                    // lookup would be rejected instead of following its referent.
                    match bytes {
                        b"ASSIGN:dir" => Ok(std::path::PathBuf::from("Work:real dir")),
                        b"RAM:" => Ok(std::path::PathBuf::from("RAM:")),
                        _ => panic!("attempted lookup of final link: {bytes:?}"),
                    }
                },
                |old, new| {
                    let expected = join_native_parent(b"Work:real dir", leaf);
                    assert_eq!(old.to_bytes(), expected.as_os_str().as_bytes());
                    assert_eq!(new.to_bytes(), b"RAM:renamed");
                    0
                },
            )
            .unwrap();
            assert_eq!(
                requested_parents,
                [b"ASSIGN:dir".to_vec(), b"RAM:".to_vec()]
            );
        }
    }

    #[test]
    fn valid_and_dangling_links_are_moved_without_touching_the_referent() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let referent = fixture.0.join("referent.txt");
        fs::write(&referent, b"referent must stay").unwrap();
        for (name, link_target) in [
            ("valid-link", referent.clone()),
            ("dangling-link", fixture.0.join("absent.txt")),
        ] {
            let source = fixture.0.join(name);
            let moved = fixture.0.join(format!("renamed-{name}"));
            symlink(&link_target, &source).unwrap();
            rename_no_replace(&source, &moved).unwrap();
            assert!(fs::symlink_metadata(&source).is_err());
            assert!(fs::symlink_metadata(&moved)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(fs::read_link(&moved).unwrap(), link_target);
            assert_eq!(fs::read(&referent).unwrap(), b"referent must stay");
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    fn blocking_rename_refuses_the_marked_ui_thread() {
        ferail_core::path_guard::mark_ui_thread();
        assert!(ferail_core::path_guard::is_ui_thread());
        assert!(std::panic::catch_unwind(|| {
            let _ = rename_no_replace(Path::new("no disk lookup"), Path::new("no mutation"));
        })
        .is_err());
        // The pure constructor remains safe on the foreground thread.
        assert_eq!(
            sibling_native_path(Path::new("RAM:old"), std::ffi::OsStr::new("new")).unwrap(),
            Path::new("RAM:new")
        );
    }
}
