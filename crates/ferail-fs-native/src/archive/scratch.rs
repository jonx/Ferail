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

/// A name that reveals nothing but still lets Quick Look pick a renderer: a
/// hash of the entry path plus its original extension.
pub fn opaque_name(entry: &str) -> String {
    let digest = blake3::hash(entry.as_bytes()).to_hex();
    let stem = &digest.as_str()[..24];
    let leaf = entry.rsplit('/').next().unwrap_or(entry);
    match leaf.rsplit_once('.') {
        // A leading dot is a hidden file, not an extension.
        Some((base, ext)) if !ext.is_empty() && !base.is_empty() => format!("{stem}.{ext}"),
        _ => stem.to_string(),
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

    #[test]
    fn opaque_name_hides_the_entry_but_keeps_its_extension() {
        let staged = opaque_name("hr/salary-review-2026.pdf");
        // The extension survives — Quick Look dispatches on it.
        assert!(staged.ends_with(".pdf"), "got {staged}");
        // Nothing of the original path does.
        assert!(!staged.contains("salary"), "got {staged}");
        assert!(!staged.contains("hr"), "got {staged}");
        // Stable for the same entry, distinct for different ones.
        assert_eq!(staged, opaque_name("hr/salary-review-2026.pdf"));
        assert_ne!(staged, opaque_name("hr/salary-review-2027.pdf"));
    }

    #[test]
    fn opaque_name_handles_entries_without_a_usable_extension() {
        assert!(!opaque_name("README").contains("README"));
        // A dot in a *directory* must not be read as the leaf's extension.
        let staged = opaque_name("v1.2/CHANGELOG");
        assert!(!staged.contains('.'), "got {staged}");
        // A dotfile is hidden, not an extension.
        let staged = opaque_name("cfg/.gitignore");
        assert!(!staged.contains(".gitignore"), "got {staged}");
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
