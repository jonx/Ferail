//! Durable in-place file rewrites shared by the built-in editors
//! (docs/features/TEXT_EDITOR.md, docs/features/IMAGE_EDITOR.md).
//!
//! Two steps: (1) the full new contents go to a unique hidden sibling, so
//! the bytes are durably on disk before the original is touched; (2) the
//! original is rewritten **in place**, same inode, so Finder tags, ACLs,
//! permissions, and the creation date all survive, which a rename-over
//! would silently drop. Then the sibling is removed. If step 2 fails
//! midway the sibling stays behind; the caller composes the user-facing
//! message and names it as the recovery copy.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) struct SafeWriteError {
    /// The underlying I/O error, already stringified.
    pub error: String,
    /// The surviving backup sibling, when the new bytes made it to disk.
    pub backup: Option<PathBuf>,
}

/// Blocking: background executor only.
pub(crate) fn write_bytes_in_place(
    path: &Path,
    bytes: &[u8],
    purpose: &str,
) -> Result<(), SafeWriteError> {
    ferail_core::path_guard::assert_off_ui_thread("safe_write::write_bytes_in_place");
    let leaf = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let tmp = path.with_file_name(format!(
        ".{leaf}.ferail-{purpose}-{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, bytes).map_err(|e| SafeWriteError {
        error: e.to_string(),
        backup: None,
    })?;
    match std::fs::write(path, bytes) {
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            Ok(())
        }
        Err(e) => Err(SafeWriteError {
            error: e.to_string(),
            backup: Some(tmp),
        }),
    }
}
