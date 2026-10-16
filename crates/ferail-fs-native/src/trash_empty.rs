//! Emptying trash directories: permanent deletion with progress.
//!
//! `std::fs::remove_dir_all` deletes a whole tree in one call, which gives a
//! caller nothing to show while a multi-gigabyte item disappears, and stops at
//! the first locked file. This walker deletes depth-first itself so it can
//! report files and bytes as it goes, and clears the user flags (`uchg`,
//! `uappnd`) that make Finder-locked items refuse deletion, the way Finder's
//! own Empty Trash does. Symlinks are deleted, never followed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::file_ops::FileOpError;

/// Running totals, reported while the trash empties.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmptyProgress {
    /// Top-level trash entries found when the run started.
    pub items_total: usize,
    /// Top-level entries finished, deleted or failed.
    pub items_done: usize,
    /// Files, symlinks and directories removed so far, at any depth.
    pub entries_deleted: u64,
    /// Logical size of the files removed so far.
    pub bytes_freed: u64,
}

/// What a run achieved.
#[derive(Debug, Default)]
pub struct EmptyOutcome {
    /// Top-level entries removed completely.
    pub items_deleted: usize,
    pub progress: EmptyProgress,
    /// One entry per top-level item that could not be removed completely.
    /// The path is the top-level item, so a retry (for example an elevated
    /// one) finishes the whole item; `raw` names the entry that refused.
    pub failures: Vec<FileOpError>,
    /// Trash directories that could not even be listed (on macOS, typically
    /// `~/.Trash` without Full Disk Access).
    pub unreadable: Vec<PathBuf>,
}

/// How often `on_progress` is called while one large item is being removed.
const REPORT_EVERY: Duration = Duration::from_millis(100);

/// Permanently delete everything inside each of `dirs` (the directories
/// themselves stay). `on_progress` is called after every top-level item and
/// at most every [`REPORT_EVERY`] in between. Blocking: call it off the UI
/// thread.
pub fn empty_dirs(dirs: &[PathBuf], on_progress: &mut dyn FnMut(&EmptyProgress)) -> EmptyOutcome {
    ferail_core::path_guard::assert_off_ui_thread("trash_empty::empty_dirs");
    let mut outcome = EmptyOutcome::default();
    let mut items = Vec::new();
    for dir in dirs {
        match std::fs::read_dir(dir) {
            Ok(entries) => items.extend(entries.flatten().map(|entry| entry.path())),
            Err(_) => outcome.unreadable.push(dir.clone()),
        }
    }
    outcome.progress.items_total = items.len();
    on_progress(&outcome.progress);

    let mut last_report = Instant::now();
    for item in items {
        let result = {
            let mut report = |progress: &EmptyProgress| {
                if last_report.elapsed() >= REPORT_EVERY {
                    last_report = Instant::now();
                    on_progress(progress);
                }
            };
            remove_tree(&item, &mut outcome.progress, &mut report)
        };
        match result {
            Ok(()) => outcome.items_deleted += 1,
            Err((error, culprit)) => {
                let mut failure = FileOpError::from_io(&error, &item);
                if culprit != item {
                    failure.raw = format!("{}: {error}", culprit.display());
                }
                outcome.failures.push(failure);
            }
        }
        outcome.progress.items_done += 1;
        last_report = Instant::now();
        on_progress(&outcome.progress);
    }
    outcome
}

type TreeError = (std::io::Error, PathBuf);

/// Remove `root` and everything under it, depth-first, without following
/// symlinks. Stops at the first entry that refuses, returning it.
fn remove_tree(
    root: &Path,
    progress: &mut EmptyProgress,
    report: &mut dyn FnMut(&EmptyProgress),
) -> Result<(), TreeError> {
    let meta = std::fs::symlink_metadata(root).map_err(|e| (e, root.to_path_buf()))?;
    if !meta.is_dir() {
        return remove_leaf(root, meta.len(), progress);
    }
    // (directory, children already pushed)
    let mut stack = vec![(root.to_path_buf(), false)];
    while let Some((dir, expanded)) = stack.pop() {
        if expanded {
            retry_unlocked(&dir, || std::fs::remove_dir(&dir)).map_err(|e| (e, dir.clone()))?;
            progress.entries_deleted += 1;
            report(progress);
            continue;
        }
        stack.push((dir.clone(), true));
        let entries = std::fs::read_dir(&dir).map_err(|e| (e, dir.clone()))?;
        for entry in entries {
            let path = entry.map_err(|e| (e, dir.clone()))?.path();
            let meta = std::fs::symlink_metadata(&path).map_err(|e| (e, path.clone()))?;
            if meta.is_dir() {
                stack.push((path, false));
            } else {
                remove_leaf(&path, meta.len(), progress)?;
                report(progress);
            }
        }
    }
    Ok(())
}

fn remove_leaf(path: &Path, len: u64, progress: &mut EmptyProgress) -> Result<(), TreeError> {
    retry_unlocked(path, || std::fs::remove_file(path)).map_err(|e| (e, path.to_path_buf()))?;
    progress.entries_deleted += 1;
    progress.bytes_freed = progress.bytes_freed.saturating_add(len);
    Ok(())
}

/// Run `op`; if it is refused, clear the user flags on `path` and on its
/// parent (removing an entry needs the parent unlocked too) and try once
/// more. System flags need root and keep failing, which is what the caller's
/// elevated retry is for.
fn retry_unlocked(path: &Path, op: impl Fn() -> std::io::Result<()>) -> std::io::Result<()> {
    match op() {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let cleared = clear_user_flags(path) | path.parent().is_some_and(clear_user_flags);
            if cleared { op() } else { Err(e) }
        }
        other => other,
    }
}

/// Clear `uchg`, `uappnd` and the other user flags, without following a
/// symlink. Returns whether anything was cleared.
#[cfg(target_os = "macos")]
fn clear_user_flags(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    use std::os::macos::fs::MetadataExt as _;
    // UF_SETTABLE: the owner-settable flags (uchg, uappnd, nodump, hidden...).
    const UF_SETTABLE: u32 = 0x0000_ffff;
    let flags = meta.st_flags();
    if flags & UF_SETTABLE == 0 {
        return false;
    }
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe extern "C" {
        // <sys/stat.h>: chflags(2) without following a final symlink. In the
        // macOS libc but not exported by the `libc` crate.
        fn lchflags(path: *const libc::c_char, flags: libc::c_uint) -> libc::c_int;
    }
    // SAFETY: `c_path` is a valid NUL-terminated path for the call's duration.
    unsafe { lchflags(c_path.as_ptr(), flags & !UF_SETTABLE) == 0 }
}

#[cfg(not(target_os = "macos"))]
fn clear_user_flags(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ferail-trash-empty-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empties_nested_items_and_counts_them() {
        let trash = scratch("nested");
        std::fs::create_dir_all(trash.join("a/b/c")).unwrap();
        std::fs::write(trash.join("a/b/c/one.bin"), vec![0u8; 1000]).unwrap();
        std::fs::write(trash.join("a/two.bin"), vec![0u8; 24]).unwrap();
        std::fs::write(trash.join("loose.txt"), b"x").unwrap();

        let mut reports = 0;
        let outcome = empty_dirs(std::slice::from_ref(&trash), &mut |_| reports += 1);

        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        assert_eq!(outcome.items_deleted, 2);
        assert_eq!(outcome.progress.items_done, 2);
        assert_eq!(outcome.progress.bytes_freed, 1025);
        // three files + directories a, a/b, a/b/c
        assert_eq!(outcome.progress.entries_deleted, 6);
        assert!(reports >= 3, "a start report and one per item");
        assert!(trash.is_dir(), "the trash directory itself stays");
        assert_eq!(std::fs::read_dir(&trash).unwrap().count(), 0);
        std::fs::remove_dir_all(&trash).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn deletes_symlinks_without_following_them() {
        let trash = scratch("symlink");
        let outside = scratch("symlink-target");
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, trash.join("link-to-dir")).unwrap();

        let outcome = empty_dirs(std::slice::from_ref(&trash), &mut |_| {});

        assert!(outcome.failures.is_empty());
        assert!(outside.join("keep.txt").exists(), "target must survive");
        std::fs::remove_dir_all(&trash).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn deletes_finder_locked_items() {
        let trash = scratch("locked");
        std::fs::create_dir_all(trash.join("folder")).unwrap();
        let locked = trash.join("folder/locked.txt");
        std::fs::write(&locked, b"locked").unwrap();
        let status = std::process::Command::new("/usr/bin/chflags")
            .arg("uchg")
            .arg(&locked)
            .status()
            .unwrap();
        assert!(status.success());

        let outcome = empty_dirs(std::slice::from_ref(&trash), &mut |_| {});

        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        assert_eq!(std::fs::read_dir(&trash).unwrap().count(), 0);
        std::fs::remove_dir_all(&trash).unwrap();
    }

    #[test]
    fn an_unreadable_trash_is_reported_not_counted() {
        let missing = std::env::temp_dir().join("ferail-trash-empty-does-not-exist");
        let outcome = empty_dirs(std::slice::from_ref(&missing), &mut |_| {});
        assert_eq!(outcome.unreadable, vec![missing]);
        assert_eq!(outcome.progress.items_total, 0);
        assert!(outcome.failures.is_empty());
    }
}
