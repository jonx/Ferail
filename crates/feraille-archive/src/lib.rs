//! Pure archive model for Feraille: format identity, the per-format capability
//! matrix, the table-of-contents entry model, and zip-slip path validation.
//! No I/O, no platform dependencies.
//!
//! The actual codec work — reading a central directory, extracting entries,
//! writing a new archive — lives in `feraille-fs-native` and runs off the UI
//! thread (Prime Directive). This crate is what both the codec layer and the
//! GPUI archive view agree on: the format enum, what each format is allowed to
//! do, and the shape of a parsed entry. Mirrors `feraille-disk-usage` (pure
//! model; scanning lives elsewhere).
//!
//! Two surfaces consume it:
//! - the quick-action Compress / Extract context-menu commands, and
//! - the embedded Archive workbench view (browse, cherry-pick, create).
//!
//! Both share this one engine and the capability matrix that tells the UI
//! which formats are read-only for editing.

pub mod capability;
pub mod entry;
pub mod format;
pub mod safety;

pub use capability::Capabilities;
pub use entry::{ArchiveEntry, Toc};
pub use format::{CompressionLevel, Format};
pub use safety::{safe_relative_path, UnsafePath};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_multipart_tar_before_single_gz() {
        assert_eq!(Format::from_path("backup.tar.gz"), Some(Format::TarGz));
        assert_eq!(Format::from_path("backup.tgz"), Some(Format::TarGz));
        assert_eq!(Format::from_path("data.gz"), Some(Format::Gzip));
        assert_eq!(Format::from_path("logs.tar.xz"), Some(Format::TarXz));
        assert_eq!(Format::from_path("logs.tar.bz2"), Some(Format::TarBz2));
    }

    #[test]
    fn detection_is_case_insensitive_and_leaf_only() {
        assert_eq!(Format::from_path("ARCHIVE.ZIP"), Some(Format::Zip));
        // A `.zip` in a parent dir name must not match the file.
        assert_eq!(Format::from_path("/some.zip/inner/readme.txt"), None);
        assert_eq!(Format::from_path("note.txt"), None);
        assert!(Format::is_archive_path("x.7z"));
        assert!(!Format::is_archive_path("x.rar")); // rar is out of scope
    }

    #[test]
    fn capability_matrix_marks_read_only_formats() {
        assert!(!Format::Zip.capabilities().is_read_only());
        assert!(Format::SevenZ.capabilities().is_read_only());
        // Tar can be created fresh, so it is not "read only" even though it
        // cannot be edited in place.
        assert!(!Format::Tar.capabilities().is_read_only());
        assert!(!Format::Tar.capabilities().can_edit_in_place);
        assert!(!Format::TarGz.capabilities().can_edit_in_place);
        // Only zip supports both a password and in-place editing.
        assert!(Format::Zip.capabilities().supports_password);
        assert!(Format::SevenZ.capabilities().supports_password);
        assert!(!Format::TarGz.capabilities().supports_password);
    }

    #[test]
    fn single_root_drives_smart_extraction() {
        let wrapped = Toc {
            entries: vec![
                dir("project/"),
                file("project/src/main.rs", 10),
                file("project/README.md", 5),
            ],
            needs_password: false,
        };
        assert_eq!(wrapped.single_root(), Some("project"));

        let loose = Toc {
            entries: vec![file("a.txt", 1), file("b.txt", 2)],
            needs_password: false,
        };
        assert_eq!(loose.single_root(), None);
        assert_eq!(loose.total_uncompressed(), Some(3));
        assert_eq!(loose.file_count(), 2);
    }

    #[test]
    fn zip_slip_rejected_absolute_and_traversal() {
        assert_eq!(safe_relative_path("../../etc/passwd"), Err(UnsafePath::Traversal));
        assert_eq!(safe_relative_path("/etc/passwd"), Err(UnsafePath::Absolute));
        assert_eq!(safe_relative_path("C:\\Windows\\x"), Err(UnsafePath::DrivePrefix));
        assert_eq!(safe_relative_path("\\\\host\\share\\x"), Err(UnsafePath::DrivePrefix));
        assert_eq!(safe_relative_path("a/../b"), Err(UnsafePath::Traversal));
        assert_eq!(safe_relative_path(""), Err(UnsafePath::Empty));
    }

    #[test]
    fn zip_slip_accepts_and_normalizes_safe_paths() {
        assert_eq!(safe_relative_path("project/src/main.rs").unwrap(), "project/src/main.rs");
        assert_eq!(safe_relative_path("./a/./b").unwrap(), "a/b");
        assert_eq!(safe_relative_path("dir\\file.txt").unwrap(), "dir/file.txt");
    }

    fn file(path: &str, size: u64) -> ArchiveEntry {
        ArchiveEntry {
            path: path.to_string(),
            is_dir: false,
            uncompressed_size: Some(size),
            compressed_size: None,
            mtime_unix: None,
            encrypted: false,
        }
    }

    fn dir(path: &str) -> ArchiveEntry {
        ArchiveEntry {
            path: path.to_string(),
            is_dir: true,
            uncompressed_size: None,
            compressed_size: None,
            mtime_unix: None,
            encrypted: false,
        }
    }
}
