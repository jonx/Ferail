//! Platform-specific filesystem paths and well-known locations.
//!
//! v1 is env-based: `$USERPROFILE` on Windows, `$HOME` on macOS/Unix.
//! Subfolder names are joined literally ("Documents", "Downloads", …),
//! matching the platform's default layout. Folder *redirection*
//! (Documents moved to OneDrive on Windows; `NSFileManager
//! URLsForDirectory` on macOS) is a Phase-2 TODO — switch backends here
//! without touching call sites.

use std::path::PathBuf;

/// The user's home directory. Reads the platform's home env var; if
/// unset, falls back to a sentinel (`C:\` / `/`) so the rest of the
/// app keeps a valid `PathBuf` instead of panicking.
pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"))
    }
}

/// One row in the sidebar's "Locations" section: a labelled shortcut
/// to a canonical user folder. The `icon` is an asset path resolved by
/// `FeraAssets` at render time.
#[derive(Debug, Clone)]
pub struct WellKnownLocation {
    pub label: &'static str,
    pub path: PathBuf,
    pub icon: &'static str,
}

/// Per-platform canonical Locations list in sidebar order. Each entry's
/// `path` is `home_dir()` joined with the platform's standard subfolder
/// name (or `home_dir()` itself for "Home").
pub fn well_known_locations() -> Vec<WellKnownLocation> {
    let home = home_dir();
    let entries: &[(&'static str, Option<&'static str>, &'static str)] = {
        #[cfg(target_os = "macos")]
        {
            &[
                ("Home", None, "icons/nav/home.svg"),
                ("Applications", Some("Applications"), "icons/nav/apps.svg"),
                ("Desktop", Some("Desktop"), "icons/nav/desktop.svg"),
                ("Documents", Some("Documents"), "icons/nav/documents.svg"),
                ("Downloads", Some("Downloads"), "icons/nav/downloads.svg"),
                ("Trash", Some(".Trash"), "icons/nav/trash.svg"),
                ("Movies", Some("Movies"), "icons/nav/movies.svg"),
                ("Music", Some("Music"), "icons/nav/music.svg"),
                ("Pictures", Some("Pictures"), "icons/nav/pictures.svg"),
            ]
        }
        #[cfg(windows)]
        {
            &[
                ("Home", None, "icons/nav/home.svg"),
                ("Desktop", Some("Desktop"), "icons/nav/desktop.svg"),
                ("Documents", Some("Documents"), "icons/nav/documents.svg"),
                ("Downloads", Some("Downloads"), "icons/nav/downloads.svg"),
                ("Pictures", Some("Pictures"), "icons/nav/pictures.svg"),
                ("Music", Some("Music"), "icons/nav/music.svg"),
                ("Videos", Some("Videos"), "icons/nav/movies.svg"),
            ]
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            &[
                ("Home", None, "icons/nav/home.svg"),
                ("Desktop", Some("Desktop"), "icons/nav/desktop.svg"),
                ("Documents", Some("Documents"), "icons/nav/documents.svg"),
                ("Downloads", Some("Downloads"), "icons/nav/downloads.svg"),
            ]
        }
    };

    entries
        .iter()
        .map(|&(label, sub, icon)| WellKnownLocation {
            label,
            path: sub.map_or_else(|| home.clone(), |s| home.join(s)),
            icon,
        })
        .collect()
}
