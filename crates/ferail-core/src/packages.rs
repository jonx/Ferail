//! macOS packages: directories that Finder presents as a single item.
//!
//! An application (`Foo.app`), an installer (`Foo.pkg`), a Photos library or
//! a Keynote document is a directory on disk, but Finder shows it as one
//! file: double-clicking it opens it, and looking inside is a separate,
//! deliberate command (Show Package Contents). Ferail follows the same rule.
//!
//! Finder decides through Launch Services (a bundle bit, or a type declared
//! as a package), which is a blocking query. This test is lexical instead, on
//! the extensions that are packages on every Mac, so it can answer during
//! row activation and menu building without touching the disk. Frameworks
//! are bundles but not packages: Finder browses into them, and so does
//! Ferail.

/// Extensions, lowercase, of the directory types Finder treats as packages.
const PACKAGE_EXTENSIONS: &[&str] = &[
    // Applications and their extensions.
    "app",
    "appex",
    "xpc",
    // Loadable bundles and plug-ins.
    "bundle",
    "plugin",
    "kext",
    "prefpane",
    "qlgenerator",
    "mdimporter",
    "saver",
    "component",
    // Installers.
    "pkg",
    "mpkg",
    // Document packages.
    "rtfd",
    "pages",
    "numbers",
    "key",
    "scptd",
    "workflow",
    "playground",
    "xcodeproj",
    "xcworkspace",
    "xcarchive",
    "logicx",
    "band",
    "fcpbundle",
    // Libraries.
    "photoslibrary",
    "musiclibrary",
    "tvlibrary",
    "imovielibrary",
];

/// Whether a directory named `name` is a package on macOS. Always `false`
/// on other platforms, where the same directory is an ordinary folder.
pub fn is_package_name(name: &str) -> bool {
    cfg!(target_os = "macos") && has_package_extension(name)
}

/// The extension test alone, on every platform. For code that reads macOS
/// volumes from elsewhere, and for tests.
pub fn has_package_extension(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && PACKAGE_EXTENSIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::has_package_extension;

    #[test]
    fn applications_and_document_packages_are_packages() {
        for name in [
            "Safari.app",
            "Installer.PKG",
            "Photos Library.photoslibrary",
            "Deck.key",
            "Notes.rtfd",
        ] {
            assert!(has_package_extension(name), "{name}");
        }
    }

    #[test]
    fn frameworks_plain_folders_and_dotfiles_are_not() {
        for name in ["Sparkle.framework", "Documents", "archive.zip", ".app", "app"] {
            assert!(!has_package_extension(name), "{name}");
        }
    }
}
