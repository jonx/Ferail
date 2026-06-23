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

/// Which root the Windows sidebar Locations resolve against when a folder
/// has been moved by OneDrive's Known-Folder-Move. OneDrive redirects some
/// folders (typically Desktop / Documents / Pictures) into the OneDrive
/// tree while leaving others (Downloads / Music / Videos) in the local
/// profile, and a leftover local copy of a redirected folder often still
/// exists — so "where is my Documents?" genuinely has two answers. This
/// lets the user pin which one the sidebar points at.
///
/// Has no effect off Windows (no Known-Folder-Move there).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpecialFolderMode {
    /// Whatever the shell reports via `SHGetKnownFolderPath` — OneDrive for
    /// the redirected folders, local for the rest. The correct default.
    #[default]
    Auto,
    /// Prefer the literal `%USERPROFILE%\<folder>` when it exists on disk,
    /// else fall back to the shell answer so the entry is never dead.
    Local,
    /// Prefer `%OneDrive%\<folder>` when it exists on disk, else fall back
    /// to the shell answer.
    OneDrive,
}

impl SpecialFolderMode {
    /// Parse the persisted `app_state` token. Unknown / missing → `Auto`.
    pub fn from_str(s: &str) -> Self {
        match s {
            "local" => SpecialFolderMode::Local,
            "onedrive" => SpecialFolderMode::OneDrive,
            _ => SpecialFolderMode::Auto,
        }
    }

    /// The token persisted in `app_state`.
    pub fn as_str(self) -> &'static str {
        match self {
            SpecialFolderMode::Auto => "auto",
            SpecialFolderMode::Local => "local",
            SpecialFolderMode::OneDrive => "onedrive",
        }
    }
}

/// The sidebar Locations in `Auto` mode — the standard entry point. Equivalent
/// to [`well_known_locations_for`]`(SpecialFolderMode::Auto)`.
pub fn well_known_locations() -> Vec<WellKnownLocation> {
    well_known_locations_for(SpecialFolderMode::default())
}

/// Per-platform canonical Locations list in sidebar order.
///
/// macOS / Unix: `home_dir()` joined with the standard subfolder name (or
/// `home_dir()` itself for "Home"). `mode` is Windows-only and ignored here.
#[cfg(not(windows))]
pub fn well_known_locations_for(_mode: SpecialFolderMode) -> Vec<WellKnownLocation> {
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
/// Desktop, so `%USERPROFILE%\Pictures` may not exist).
///
/// `mode` picks which root a redirected folder points at (see
/// [`SpecialFolderMode`]). `Local` / `OneDrive` only override the shell
/// answer when their preferred path *exists on disk* — otherwise they fall
/// back to the shell path, so the sidebar never shows an entry that opens
/// to nothing. The shell answer itself is the final fallback to the literal
/// `home\<sub>` when the folder can't be resolved at all.
#[cfg(windows)]
pub fn well_known_locations_for(mode: SpecialFolderMode) -> Vec<WellKnownLocation> {
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music,
        FOLDERID_Pictures, FOLDERID_Videos,
    };

    let home = home_dir();
    // OneDrive root for `OneDrive` mode. `OneDriveCommercial` is the
    // work/school sync root, `OneDrive` the primary one of whichever kind,
    // `OneDriveConsumer` the personal one — prefer the most specific that's
    // set. `None` when the machine has no OneDrive (then `OneDrive` mode
    // simply behaves like `Auto`).
    let onedrive_root = std::env::var_os("OneDriveCommercial")
        .or_else(|| std::env::var_os("OneDrive"))
        .or_else(|| std::env::var_os("OneDriveConsumer"))
        .map(PathBuf::from);

    let mut out = vec![WellKnownLocation {
        // Home is the profile dir itself — never redirected, so `mode`
        // doesn't touch it.
        label: "Home",
        path: home.clone(),
        icon: "icons/nav/home.svg",
    }];

    // (label, known-folder id, subfolder name, icon)
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

    for &(label, fid, sub, icon) in specs {
        // The shell's answer (OneDrive-aware) is the baseline and the
        // ultimate fallback for every mode.
        let auto = known_folder_path(fid).unwrap_or_else(|| home.join(sub));
        // A mode-preferred root only wins if it actually exists on disk;
        // `prefer_if_exists` keeps every sidebar entry pointing somewhere real.
        let path = match mode {
            SpecialFolderMode::Auto => auto,
            SpecialFolderMode::Local => prefer_if_exists(home.join(sub), auto),
            SpecialFolderMode::OneDrive => match &onedrive_root {
                Some(root) => prefer_if_exists(root.join(sub), auto),
                None => auto,
            },
        };
        out.push(WellKnownLocation { label, path, icon });
    }
    out
}

/// Return `preferred` when it resolves to an existing directory, else
/// `fallback`. The single `is_dir` stat here is why resolution is computed
/// off the render path (once at startup / on a settings change) and cached —
/// `render` must never stat (the Prime Directive's OneDrive-placeholder rule).
#[cfg(windows)]
fn prefer_if_exists(preferred: PathBuf, fallback: PathBuf) -> PathBuf {
    if preferred.is_dir() {
        preferred
    } else {
        fallback
    }
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

    /// The Local / OneDrive override modes must never leave a sidebar entry
    /// pointing at a folder that doesn't exist: when the preferred root has
    /// no such folder they fall back to the shell answer. Every label that
    /// resolves to a directory in `Auto` must still resolve to one in the
    /// other two modes (a redirected folder always exists somewhere).
    #[test]
    fn override_modes_never_produce_dead_entries() {
        let auto = well_known_locations_for(SpecialFolderMode::Auto);
        for mode in [SpecialFolderMode::Local, SpecialFolderMode::OneDrive] {
            let locs = well_known_locations_for(mode);
            assert_eq!(locs.len(), auto.len(), "{mode:?} keeps every Location");
            for (a, l) in auto.iter().zip(&locs) {
                assert_eq!(a.label, l.label, "Location order is stable across modes");
                if a.path.is_dir() {
                    assert!(
                        l.path.is_dir(),
                        "{mode:?} {} resolves to a real dir: {:?}",
                        l.label,
                        l.path
                    );
                }
            }
        }
    }
}
