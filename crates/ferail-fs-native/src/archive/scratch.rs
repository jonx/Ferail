//! Scratch storage for archive entries staged out for preview.
//!
//! Previewing an entry means writing it out in the clear: Quick Look is a
//! separate OS process that reads a file URL, so an encrypted or hashed
//! *payload* could not be rendered at all. What we can control is everything
//! around it, and this module owns those decisions:
//!
//! - Staging happens under [`std::env::temp_dir`], which on macOS is the
//!   per-user `$TMPDIR` (`/var/folders/…/T`, mode 0700), not world-readable
//!   `/tmp`.
//! - Directories are created 0700 and staged files 0600, so no other user on
//!   a shared machine can read them.
//! - Staged files are named by a hash of the entry path ([`opaque_name`]), so
//!   a leftover file leaks nothing through its *name* — "salary-review.pdf" is
//!   metadata even when the bytes are unreadable.
//! - Each directory carries the owning process's PID, and
//!   [`sweep_stale_scratch`] removes the directories of processes that are no
//!   longer running. A clean exit is handled by the caller's `Drop`; the sweep
//!   is what covers crashes and kills, which no in-process cleanup can.

use std::path::{Path, PathBuf};

/// Prefix for per-process scratch directories. [`sweep_stale_scratch`] relies
/// on the trailing PID.
pub const SCRATCH_PREFIX: &str = "ferail-archive-preview-";

/// This process's scratch directory, created 0700 if absent.
pub fn scratch_dir() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("{SCRATCH_PREFIX}{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    set_private_permissions(&dir, 0o700);
    Some(dir)
}

/// The one path an entry is ever staged to, inside `dir`.
///
/// Always `preview.<ext>` — the extension is kept because Quick Look
/// dispatches on it, and the stem is constant so the filename says nothing
/// about the entry.
///
/// Pure: call [`clear_staged_dir`] before extracting to guarantee only one
/// staged file exists. (Wiping from *here* would delete the freshly extracted
/// entry this path is meant to receive.)
pub fn staged_path(dir: &Path, entry: &str) -> PathBuf {
    let leaf = entry.rsplit('/').next().unwrap_or(entry);
    match leaf.rsplit_once('.') {
        // A leading dot is a hidden file, not an extension.
        Some((base, ext)) if !ext.is_empty() && !base.is_empty() => {
            dir.join(format!("preview.{ext}"))
        }
        _ => dir.join("preview"),
    }
}

/// Remove everything previously staged in `dir`, best-effort.
pub fn clear_staged_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Restrict a staged file or directory to its owner. No-op off unix, where the
/// per-user temp directory is the protection.
pub fn set_private_permissions(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

/// Remove a single staged file, best-effort.
///
/// Callers stage at most one entry at a time and drop the previous one as soon
/// as it is superseded, so the window in which any archive content exists in
/// the clear is bounded to "while you are looking at it" rather than "until
/// the app exits".
pub fn remove_staged(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Delete scratch directories left behind by processes that are no longer
/// running. Call once at startup.
pub fn sweep_stale_scratch() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(pid) = name.strip_prefix(SCRATCH_PREFIX) else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else { continue };
        if pid == me || process_is_alive(pid) {
            continue;
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

/// Whether `pid` is still running. `kill(pid, 0)` reports liveness without
/// signalling; off unix we keep the directory rather than risk deleting a live
/// process's scratch.
fn process_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 performs error checking only — it never delivers.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private directory per test. `scratch_dir()` is keyed by PID, so every
    /// test in this process would otherwise share — and delete — the same one.
    fn test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferail-scratch-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn staged_path_keeps_the_extension_but_never_the_name() {
        let dir = test_dir("ext");
        let p = staged_path(&dir, "hr/salary-review-2026.pdf");
        // The extension survives — Quick Look dispatches on it.
        assert_eq!(p.extension().unwrap(), "pdf");
        // Nothing of the original path does.
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name, "preview.pdf", "the stem must be constant");
        // Two different entries of the same type share the one path, so they
        // cannot accumulate.
        assert_eq!(p, staged_path(&dir, "finance/board-minutes.pdf"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staged_path_handles_entries_without_a_usable_extension() {
        let dir = test_dir("noext");
        assert_eq!(staged_path(&dir, "README").file_name().unwrap(), "preview");
        // A dot in a *directory* must not be read as the leaf's extension.
        assert_eq!(
            staged_path(&dir, "v1.2/CHANGELOG").file_name().unwrap(),
            "preview"
        );
        // A dotfile is hidden, not an extension.
        assert_eq!(
            staged_path(&dir, "cfg/.gitignore").file_name().unwrap(),
            "preview"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clearing_removes_previous_files_and_extracted_subdirs() {
        let dir = test_dir("clear");
        let first = staged_path(&dir, "secret/notes.txt");
        std::fs::write(&first, b"private").unwrap();
        // Extraction of a nested entry leaves a directory behind too.
        std::fs::create_dir_all(dir.join("nested/deep")).unwrap();
        std::fs::write(dir.join("nested/deep/x.bin"), b"x").unwrap();

        clear_staged_dir(&dir);

        assert!(!first.exists(), "previous staged file must be gone");
        assert!(!dir.join("nested").exists(), "extracted dirs must go too");
        assert!(dir.exists(), "the scratch dir itself stays");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_staged_deletes_the_file_and_tolerates_a_missing_one() {
        let dir = test_dir("remove");
        let f = dir.join("preview.txt");
        std::fs::write(&f, b"private").unwrap();
        remove_staged(&f);
        assert!(!f.exists(), "staged file should be gone");
        // Superseding an already-removed file must not panic.
        remove_staged(&f);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_removes_dead_process_scratch_but_spares_our_own() {
        // A crash leaves a scratch dir behind; the next launch must take it.
        let dead = std::env::temp_dir().join(format!("{SCRATCH_PREFIX}4294967290"));
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::write(dead.join("leftover"), b"private").unwrap();
        let mine = scratch_dir().expect("our own scratch");

        sweep_stale_scratch();

        assert!(!dead.exists(), "scratch of a dead process should be removed");
        assert!(mine.exists(), "our own live scratch must be spared");
        let _ = std::fs::remove_dir_all(&mine);
    }
}
