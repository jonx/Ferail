//! Platform-specific filesystem paths and well-known locations.
//!
//! Home is env-based (`$USERPROFILE` on Windows, `$HOME` elsewhere). The
//! sidebar's "Locations" are resolved per platform: on Windows via
//! `SHGetKnownFolderPath`, which returns the *real* (possibly OneDrive-
//! redirected / moved) folder rather than a literal `%USERPROFILE%\Pictures`
//! that may not exist; on macOS/Unix by joining the standard subfolder name.

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

/// Per-platform canonical Locations list in sidebar order.
///
/// macOS / Unix: `home_dir()` joined with the standard subfolder name (or
/// `home_dir()` itself for "Home").
#[cfg(not(windows))]
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
        #[cfg(not(target_os = "macos"))]
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

/// Windows: resolve each Location via `SHGetKnownFolderPath` so the path
/// reflects any redirection (OneDrive often *moves* Documents / Pictures /
/// Desktop, so `%USERPROFILE%\Pictures` may not exist). Falls back to the
/// literal `home\<sub>` if the shell can't resolve a folder.
#[cfg(windows)]
pub fn well_known_locations() -> Vec<WellKnownLocation> {
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music,
        FOLDERID_Pictures, FOLDERID_Videos,
    };

    let home = home_dir();
    let mut out = vec![WellKnownLocation {
        label: "Home",
        path: home.clone(),
        icon: "icons/nav/home.svg",
    }];

    // (label, known-folder id, fallback subfolder, icon)
    let specs: &[(&'static str, &windows::core::GUID, &'static str, &'static str)] = &[
        (
            "Desktop",
            &FOLDERID_Desktop,
            "Desktop",
            "icons/nav/desktop.svg",
        ),
        (
            "Documents",
            &FOLDERID_Documents,
            "Documents",
            "icons/nav/documents.svg",
        ),
        (
            "Downloads",
            &FOLDERID_Downloads,
            "Downloads",
            "icons/nav/downloads.svg",
        ),
        (
            "Pictures",
            &FOLDERID_Pictures,
            "Pictures",
            "icons/nav/pictures.svg",
        ),
        ("Music", &FOLDERID_Music, "Music", "icons/nav/music.svg"),
        ("Videos", &FOLDERID_Videos, "Videos", "icons/nav/movies.svg"),
    ];

    for &(label, fid, fallback, icon) in specs {
        let path = known_folder_path(fid).unwrap_or_else(|| home.join(fallback));
        out.push(WellKnownLocation { label, path, icon });
    }
    out
}

/// Resolve a Windows known folder to its current filesystem path (honours
/// OneDrive/redirection). `None` if the shell can't resolve it.
#[cfg(windows)]
fn known_folder_path(rfid: &windows::core::GUID) -> Option<PathBuf> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};

    unsafe {
        let pwstr = SHGetKnownFolderPath(rfid, KF_FLAG_DEFAULT, HANDLE::default()).ok()?;
        if pwstr.is_null() {
            return None;
        }
        let path = pwstr.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(pwstr.0 as *const core::ffi::c_void));
        path
    }
}

#[cfg(all(test, windows))]
mod win_tests {
    use super::*;

    /// On Windows the sidebar Locations must resolve through SHGetKnownFolderPath
    /// so OneDrive-redirected/moved folders point at the real path. Before this
    /// fix, `%USERPROFILE%\Pictures` often didn't exist on OneDrive boxes.
    #[test]
    fn known_locations_resolve_to_existing_dirs() {
        let locs = well_known_locations();
        let pics = locs
            .iter()
            .find(|l| l.label == "Pictures")
            .expect("Pictures location present");
        assert!(
            pics.path.is_dir(),
            "Pictures resolves to an existing directory: {:?}",
            pics.path
        );
        // Home is always the profile dir and must exist.
        let home = locs.iter().find(|l| l.label == "Home").unwrap();
        assert!(home.path.is_dir(), "Home exists: {:?}", home.path);
    }
}
