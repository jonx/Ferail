//! Which locations Ferail is allowed to remember.
//!
//! A pure path-prefix policy shared by the two things that write paths to
//! disk: the metadata store (`ferail-meta`) and the saved session
//! (`ferail-gpui`'s app state). A location that is covered is browsed like
//! any other; nothing about it is recorded.
//!
//! Matching is lexical and component-wise (`/a/b` covers `/a/b/c`, never
//! `/a/bc`), so it costs no I/O and is safe on any thread.

use std::path::{Path, PathBuf};

/// The set of path prefixes nothing is recorded under.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exclusions {
    prefixes: Vec<PathBuf>,
}

impl Exclusions {
    /// Build from user folders plus, when `other_volumes` is set, the
    /// platform's mount roots for removable, network and disk-image
    /// volumes ([`volume_roots`]). Empty and duplicate prefixes are dropped.
    pub fn new(folders: impl IntoIterator<Item = PathBuf>, other_volumes: bool) -> Self {
        let mut prefixes: Vec<PathBuf> = Vec::new();
        let roots = other_volumes.then(volume_roots).unwrap_or_default();
        for prefix in folders.into_iter().chain(roots) {
            if !prefix.as_os_str().is_empty() && !prefixes.contains(&prefix) {
                prefixes.push(prefix);
            }
        }
        Self { prefixes }
    }

    pub fn is_empty(&self) -> bool {
        self.prefixes.is_empty()
    }

    /// The excluded roots, user folders first.
    pub fn prefixes(&self) -> &[PathBuf] {
        &self.prefixes
    }

    /// Whether `path` is an excluded folder or lies inside one.
    pub fn covers(&self, path: &Path) -> bool {
        self.prefixes.iter().any(|prefix| covers(prefix, path))
    }
}

fn covers(prefix: &Path, path: &Path) -> bool {
    if path.starts_with(prefix) {
        return true;
    }
    // A UNC share is a `Prefix` component, so `\\` on its own never
    // `starts_with`-matches one; compare the spelling instead.
    cfg!(windows)
        && prefix.as_os_str() == "\\\\"
        && path.as_os_str().to_string_lossy().starts_with("\\\\")
        && !path.as_os_str().to_string_lossy().starts_with("\\\\?\\")
}

/// Where the platform mounts volumes other than the system disk.
///
/// macOS mounts every additional volume under `/Volumes`: external and
/// network disks, disk images, and also a second internal disk. Linux
/// desktops use `/media`, `/run/media` and `/mnt`. On Windows only network
/// shares are recognisable from the path alone; a removable drive has an
/// ordinary drive letter and is excluded by adding it as a folder.
pub fn volume_roots() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        vec![PathBuf::from("/Volumes")]
    }
    #[cfg(windows)]
    {
        vec![PathBuf::from("\\\\")]
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        vec![
            PathBuf::from("/media"),
            PathBuf::from("/run/media"),
            PathBuf::from("/mnt"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_policy_covers_nothing() {
        let none = Exclusions::default();
        assert!(none.is_empty());
        assert!(!none.covers(Path::new("/anything")));
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_covers_itself_and_its_descendants_only() {
        let policy = Exclusions::new([PathBuf::from("/home/u/private")], false);
        assert!(policy.covers(Path::new("/home/u/private")));
        assert!(policy.covers(Path::new("/home/u/private/a/b.txt")));
        assert!(!policy.covers(Path::new("/home/u/private-notes")));
        assert!(!policy.covers(Path::new("/home/u")));
    }

    #[test]
    fn blank_and_repeated_folders_are_dropped() {
        let policy = Exclusions::new(
            [PathBuf::new(), PathBuf::from("/a"), PathBuf::from("/a")],
            false,
        );
        assert_eq!(policy.prefixes, vec![PathBuf::from("/a")]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn other_volumes_covers_the_mount_root() {
        let policy = Exclusions::new([], true);
        assert!(policy.covers(Path::new("/Volumes/Backup/photos")));
        assert!(!policy.covers(Path::new("/Users/u/Documents")));
    }
}
