//! Cheap file-category classification (extension only). No I/O, no
//! magic-byte sniffing — magic detection is a separate worker that posts
//! its results into [`FileEntry::display_magic`] when it lands.
//!
//! macOS additions over the Ferail original: `pages`/`numbers`/`key` join
//! the document family; `app`/`bundle`/`framework`/`plugin`/`kext`/`xcodeproj`
//! count as executables (they're directory packages, but we treat them
//! as leaves at the scanner boundary).

use std::path::Path;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FileCategory {
    Image,
    Video,
    Audio,
    Archive,
    Document,
    Executable,
    Other,
}

pub fn classify_path(path: &Path) -> FileCategory {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return FileCategory::Other;
    };
    classify_extension(ext)
}

pub fn classify_extension(ext: &str) -> FileCategory {
    // The overwhelmingly common case is already-lowercase. Avoid allocating
    // one temporary `String` per file while Disk Usage classifies millions of
    // rows; retain case-insensitive behavior for the uncommon uppercase case.
    let folded;
    let lower = if ext.bytes().any(|byte| byte.is_ascii_uppercase()) {
        folded = ext.to_ascii_lowercase();
        folded.as_str()
    } else {
        ext
    };
    match lower {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "heic" | "heif"
        | "svg" | "ico" => FileCategory::Image,
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" | "mpg" | "mpeg" => {
            FileCategory::Video
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "wma" | "aiff" | "alac" => {
            FileCategory::Audio
        }
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "tgz" | "tbz" | "dmg" | "iso" => {
            FileCategory::Archive
        }
        "pdf" | "doc" | "docx" | "txt" | "rtf" | "md" | "ppt" | "pptx" | "xls" | "xlsx"
        | "pages" | "numbers" | "key" | "epub" | "odt" | "ods" | "odp" => FileCategory::Document,
        "exe" | "msi" | "bat" | "cmd" | "ps1" | "sh" | "com" | "app" | "bundle" | "framework"
        | "plugin" | "kext" | "xcodeproj" => FileCategory::Executable,
        _ => FileCategory::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn extension_lookup_basics() {
        assert_eq!(classify_extension("PNG"), FileCategory::Image);
        assert_eq!(classify_extension("Mp3"), FileCategory::Audio);
        assert_eq!(classify_extension("rs"), FileCategory::Other);
    }

    #[test]
    fn classify_path_no_extension_is_other() {
        let p = PathBuf::from("/tmp/Makefile");
        assert_eq!(classify_path(&p), FileCategory::Other);
    }

    #[test]
    fn classify_path_uses_extension() {
        let p = PathBuf::from("/x/y.PDF");
        assert_eq!(classify_path(&p), FileCategory::Document);
    }

    #[test]
    fn mac_packages_are_executables() {
        assert_eq!(classify_extension("app"), FileCategory::Executable);
        assert_eq!(classify_extension("framework"), FileCategory::Executable);
        assert_eq!(classify_extension("xcodeproj"), FileCategory::Executable);
    }

    #[test]
    fn iwork_documents_are_documents() {
        assert_eq!(classify_extension("pages"), FileCategory::Document);
        assert_eq!(classify_extension("numbers"), FileCategory::Document);
        assert_eq!(classify_extension("key"), FileCategory::Document);
    }
}
