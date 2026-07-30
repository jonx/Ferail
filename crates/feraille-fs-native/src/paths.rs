//! Platform-specific filesystem paths and well-known locations.
//!
//! Home is env-based (`$USERPROFILE` on Windows, `$HOME` elsewhere). The
//! sidebar's "Locations" are resolved per platform: on Windows via
//! `SHGetKnownFolderPath`, which returns the *real* (possibly OneDrive-
//! redirected / moved) folder rather than a literal `%USERPROFILE%\Pictures`
//! that may not exist; on macOS/Unix by joining the standard subfolder name.

use std::borrow::Cow;
use std::path::PathBuf;

/// Convert a raw on-disk filename *leaf* to the form the user should see.
///
/// macOS inherits two path separators — the colon (`:`) from classic Mac OS /
/// HFS+ and the slash (`/`) from Unix / NeXTSTEP. The POSIX layer stores a
/// colon *inside* a name component where Finder shows a slash, so a file
/// `ls` reports as `a:b` is presented by Finder as `a/b`. We mirror Finder:
/// the byte on disk stays the colon (the truth used for path ops), but every
/// user-facing surface shows the slash. See
/// <https://www.osnews.com/story/145356/a-tale-of-two-path-separators/>.
///
/// Operates on a single leaf, never a full path — the caller has already split
/// off the component. Returns `Cow::Borrowed` when nothing changes (the common
/// case), so the no-alloc-on-paint contract holds for clean names. Other
/// platforms are the identity today; this is the seam where future
/// per-platform display quirks plug in.
pub fn display_leaf(raw: &str) -> Cow<'_, str> {
    #[cfg(target_os = "macos")]
    {
        if raw.contains(':') {
            return Cow::Owned(raw.replace(':', "/"));
        }
    }
    Cow::Borrowed(raw)
}

/// Convert a *whole* path to its user-facing string form, stripping the
/// Windows extended-length (`\\?\`) prefix that must never reach the user's
/// eyes.
///
/// `std::fs::canonicalize` returns verbatim paths on Windows (the file list
/// navigates with them because the shell/trash APIs are fed the stripped form
/// elsewhere), so a raw `to_string_lossy()` in a window title, breadcrumb root,
/// or Get Info "Where" row leaks `\\?\C:\…`. This normalizes it:
/// `\\?\C:\x` → `C:\x`, `\\?\UNC\srv\share\x` → `\\srv\share\x`. Non-verbatim
/// paths and every non-Windows path are returned unchanged. Whole-path, unlike
/// the leaf-only [`display_leaf`]; the two compose (strip the prefix here, swap
/// per-leaf separators there).
pub fn display_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            if let Some(unc) = rest.strip_prefix(r"UNC\") {
                return format!(r"\\{unc}");
            }
            return rest.to_string();
        }
    }
    s.into_owned()
}

/// Validate a *typed* filename leaf before it is used to create or rename a
/// file, returning a user-facing error when the platform would reject it or
/// silently transform it into a different (often inaccessible) name.
///
/// On Windows this catches the three classic footguns:
/// - **Reserved characters** `< > : " | ? *`, the separators `\` `/`, and
///   control chars — the filesystem rejects them outright.
/// - **Reserved DOS device names** (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`,
///   `LPT1`–`LPT9`), with or without an extension (`CON.txt` is reserved too):
///   creating one either fails or yields a handle to the device.
/// - **Trailing dot or space**: Windows strips these, so `report.` silently
///   becomes `report` and `data ` becomes an inaccessible sibling.
///
/// Off Windows this is the identity (`Ok(())`) — POSIX only forbids `/` and NUL
/// in a leaf, and the caller has already split off the component. Operates on a
/// single leaf, never a full path.
pub fn validate_leaf(name: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        if let Some(bad) = name
            .chars()
            .find(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' | '/') || (*c as u32) < 0x20)
        {
            return Err(match bad {
                '\\' | '/' => "A name can’t contain a slash.".to_string(),
                c if (c as u32) < 0x20 => "A name can’t contain control characters.".to_string(),
                c => format!("A name can’t contain the {c} character."),
            });
        }
        if name.ends_with('.') || name.ends_with(' ') {
            return Err("A name can’t end with a space or a period.".to_string());
        }
        let stem = name.split('.').next().unwrap_or(name);
        if is_reserved_dos_name(stem) {
            return Err(format!("“{stem}” is a reserved Windows device name."));
        }
    }
    #[cfg(not(windows))]
    let _ = name;
    Ok(())
}

/// Whether `stem` (a filename's base, before any extension) is a reserved DOS
/// device name — case-insensitive `CON`/`PRN`/`AUX`/`NUL` or `COM`/`LPT`
/// followed by a single `1`–`9`.
#[cfg(windows)]
fn is_reserved_dos_name(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    for prefix in ["COM", "LPT"] {
        if let Some(rest) = upper.strip_prefix(prefix) {
            if rest.len() == 1 && matches!(rest.as_bytes()[0], b'1'..=b'9') {
                return true;
            }
        }
    }
    false
}

/// Inverse of [`display_leaf`]: convert a *typed* (display-form) filename leaf
/// to the bytes to write on disk.
///
/// On macOS a user who types `/` in a rename / New-Folder field means the
/// Finder-displayed slash, which is stored as a colon — exactly what Finder
/// does. This also removes a footgun: without the swap, a typed `/` reaches
/// `rename(2)` / `mkdir(2)` as a real separator and either errors (parent
/// component missing) or silently retargets an existing subdirectory.
///
/// Leaf-only, like [`display_leaf`]; `display_leaf(on_disk_leaf(x)) == x` on
/// every platform (round-trip tested below).
pub fn on_disk_leaf(typed: &str) -> Cow<'_, str> {
    #[cfg(target_os = "macos")]
    {
        if typed.contains('/') {
            return Cow::Owned(typed.replace('/', ":"));
        }
    }
    Cow::Borrowed(typed)
}

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
    // Intentional inherent method (infallible token parse), not `std::str::FromStr`.
    #[allow(clippy::should_implement_trait)]
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
/// AROS: the Unix "home joined with Desktop/Documents/Downloads" scheme does
/// not apply — those drawers do not exist on a stock AROS volume, and
/// navigating into a missing path currently trips a `posixc.library` open()
/// fault. List the always-present system roots plus any standard `SYS:`
/// drawers that actually exist, filtered so a dead row can never be clicked.
/// The mounted volumes themselves surface separately via [`crate::list_volumes`]
/// (the sidebar's Volumes section). `mode` is Windows-only and ignored.
#[cfg(target_os = "aros")]
pub fn well_known_locations_for(_mode: SpecialFolderMode) -> Vec<WellKnownLocation> {
    let candidates: &[(&'static str, &'static str, &'static str)] = &[
        ("System", "SYS:", "icons/nav/drive.svg"),
        ("Ram Disk", "RAM:", "icons/nav/drive.svg"),
        ("Prefs", "SYS:Prefs", "icons/nav/folder.svg"),
        ("Utilities", "SYS:Utilities", "icons/nav/folder.svg"),
        ("Tools", "SYS:Tools", "icons/nav/folder.svg"),
        ("Storage", "SYS:Storage", "icons/nav/folder.svg"),
        // The boot volume's Amiga Trashcan drawer — appears once something
        // has been trashed (Move to Trash creates it). Per-volume trashcans
        // for other volumes are reachable from those volumes' roots.
        ("Trash", "SYS:Trashcan", "icons/nav/trash.svg"),
    ];
    candidates
        .iter()
        .filter(|(_, path, _)| std::path::Path::new(path).exists())
        .map(|&(label, path, icon)| WellKnownLocation {
            label,
            path: PathBuf::from(path),
            icon,
        })
        .collect()
}

/// `true` when `dir` exists and holds at least one non-dotfile entry.
///
/// Used to decide whether an optional sidebar row is worth drawing at all, so
/// a location that resolves to an empty-looking folder can be dropped instead
/// of misleading the user. Reads one directory and short-circuits on the first
/// hit, and is only ever reached from [`well_known_locations_for`] — i.e. the
/// startup / settings-change resolve, never render.
#[cfg(target_os = "macos")]
fn dir_has_visible_entry(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut entries| {
        entries.any(|e| e.is_ok_and(|e| !e.file_name().as_encoded_bytes().starts_with(b".")))
    })
}

#[cfg(all(not(windows), not(target_os = "aros")))]
pub fn well_known_locations_for(_mode: SpecialFolderMode) -> Vec<WellKnownLocation> {
    let home = home_dir();
    let entries: &[(&'static str, Option<&'static str>, &'static str)] = {
        #[cfg(target_os = "macos")]
        {
            &[
                ("Home", None, "icons/nav/home.svg"),
                // Absolute on purpose: Finder's sidebar "Applications" is the
                // system-wide `/Applications`, not `~/Applications`. The
                // per-user folder is surfaced separately below when it holds
                // anything.
                ("Applications", Some("/Applications"), "icons/nav/apps.svg"),
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

    // `mut` feeds the macOS-only iCloud Drive push below.
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut locations: Vec<WellKnownLocation> = entries
        .iter()
        .map(|&(label, sub, icon)| WellKnownLocation {
            label,
            // An *absolute* `sub` (macOS `/Applications`) deliberately escapes
            // the home base — `Path::join` discards the base when the joined
            // component is absolute. Do not "fix" it to a relative name.
            path: sub.map_or_else(|| home.clone(), |s| home.join(s)),
            icon,
        })
        .collect();

    // `~/Applications` (macOS): a genuinely separate app folder that Finder's
    // sidebar hides — per-user installs plus the Chrome/Edge PWA shims. Shown
    // right under the system row, but only when it holds something the user
    // would recognise: on many Macs it is absent, or contains nothing but
    // `.localized` and would be a junk row. Reads one directory, on the same
    // startup / settings-change path as the iCloud probe below — never render.
    #[cfg(target_os = "macos")]
    {
        let user_apps = home.join("Applications");
        if dir_has_visible_entry(&user_apps) {
            let at = locations
                .iter()
                .position(|l| l.label == "Applications")
                .map_or(locations.len(), |i| i + 1);
            locations.insert(
                at,
                WellKnownLocation {
                    label: "User Applications",
                    path: user_apps,
                    icon: "icons/nav/apps.svg",
                },
            );
        }
    }

    // iCloud Drive (macOS): the ubiquity container root. Only surfaced
    // when it actually exists — a user with iCloud Drive disabled has no
    // such folder, and an unconditional dead row would mislead. This
    // resolves once at startup (cached in `special_folders`), never on
    // the render/hit-test path, so the single `exists()` is Prime-
    // Directive-safe here.
    #[cfg(target_os = "macos")]
    {
        let icloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
        if icloud.is_dir() {
            locations.push(WellKnownLocation {
                label: "iCloud Drive",
                path: icloud,
                icon: "icons/nav/cloud.svg",
            });
        }
    }

    locations
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

#[cfg(test)]
mod leaf_tests {
    use super::*;

    #[test]
    fn clean_leaf_is_borrowed_unchanged() {
        // No separator chars → identity, and no allocation.
        assert!(matches!(display_leaf("report.pdf"), Cow::Borrowed("report.pdf")));
        assert!(matches!(on_disk_leaf("report.pdf"), Cow::Borrowed("report.pdf")));
    }

    #[test]
    fn on_disk_names_round_trip_through_display() {
        // An on-disk leaf never contains `/` (it's the POSIX separator), so
        // showing it then mapping back to disk is lossless on every platform.
        for disk in ["a:b", "plain.txt", "Q1:Q2:report", "x y.txt"] {
            assert_eq!(
                on_disk_leaf(&display_leaf(disk)),
                disk,
                "on-disk round-trip for {disk:?}"
            );
        }
    }

    #[test]
    fn typed_names_round_trip_through_disk() {
        // A name the user types in a rename/New-Folder field carries no `:`
        // (on macOS that's the displayed-as-`/` separator); storing it then
        // re-displaying is lossless on every platform.
        for typed in ["a/b", "plain.txt", "2024/2025 budget", "x y.txt"] {
            assert_eq!(
                display_leaf(&on_disk_leaf(typed)),
                typed,
                "typed round-trip for {typed:?}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_swaps_colon_and_slash() {
        // Finder parity: on-disk colon shows as slash; typed slash stores a colon.
        assert_eq!(display_leaf("a:b"), "a/b");
        assert_eq!(on_disk_leaf("a/b"), "a:b");
        assert_eq!(display_leaf("Q1:Q2:report"), "Q1/Q2/report");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_is_identity() {
        // Off macOS the colon/slash have their normal meaning; no swap.
        assert_eq!(display_leaf("a:b"), "a:b");
        assert_eq!(on_disk_leaf("a:b"), "a:b");
    }

    use std::path::Path;

    #[test]
    fn display_path_leaves_plain_paths_unchanged() {
        // A path with no verbatim prefix round-trips as-is on every platform.
        let p = Path::new(if cfg!(windows) { r"C:\Users\me\file.txt" } else { "/home/me/file.txt" });
        assert_eq!(display_path(p), p.to_string_lossy());
    }

    #[cfg(windows)]
    #[test]
    fn display_path_strips_verbatim_disk_prefix() {
        assert_eq!(display_path(Path::new(r"\\?\C:\Source\feraille")), r"C:\Source\feraille");
        assert_eq!(display_path(Path::new(r"\\?\C:\")), r"C:\");
    }

    #[cfg(windows)]
    #[test]
    fn display_path_rewrites_verbatim_unc_prefix() {
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\dir")),
            r"\\server\share\dir"
        );
    }

    #[test]
    fn validate_leaf_accepts_ordinary_names() {
        for ok in ["report.pdf", "My Folder", "2024-budget", "notes"] {
            assert!(validate_leaf(ok).is_ok(), "{ok:?} should be valid");
        }
    }

    #[cfg(windows)]
    #[test]
    fn validate_leaf_rejects_reserved_chars() {
        for bad in [r"a<b", r"a>b", "a:b", "a\"b", "a|b", "a?b", "a*b", r"a\b", "a/b"] {
            assert!(validate_leaf(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[cfg(windows)]
    #[test]
    fn validate_leaf_rejects_reserved_device_names() {
        // Bare, any case, and with an extension are all reserved.
        for bad in ["CON", "con", "NUL", "PRN", "AUX", "COM1", "LPT9", "CON.txt", "nul.log"] {
            assert!(validate_leaf(bad).is_err(), "{bad:?} should be rejected");
        }
        // COM0 / LPT0 and multi-digit are NOT reserved device names.
        for ok in ["COM0", "LPT0", "COM10", "CONSOLE", "COMET"] {
            assert!(validate_leaf(ok).is_ok(), "{ok:?} should be allowed");
        }
    }

    #[cfg(windows)]
    #[test]
    fn validate_leaf_rejects_trailing_dot_or_space() {
        assert!(validate_leaf("report.").is_err());
        assert!(validate_leaf("data ").is_err());
        assert!(validate_leaf(".").is_err());
        assert!(validate_leaf("..").is_err());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod mac_tests {
    use super::*;

    /// The sidebar's "Applications" must be the system-wide `/Applications`,
    /// like Finder's — not `~/Applications`, which on most Macs holds only PWA
    /// shims and would make the row look near-empty. Regression guard: the row
    /// is spelled as an absolute join, which reads like a typo.
    #[test]
    fn applications_is_the_system_folder() {
        let locs = well_known_locations();
        let apps = locs
            .iter()
            .find(|l| l.label == "Applications")
            .expect("Applications location present");
        assert_eq!(apps.path, PathBuf::from("/Applications"));
    }

    /// `~/Applications` gets its own row, directly after the system one, and
    /// only when it holds a visible entry. Skipped on a machine that has no
    /// such folder (or an empty one) — there the absence *is* the contract.
    #[test]
    fn user_applications_row_is_optional_and_adjacent() {
        let locs = well_known_locations();
        let Some(at) = locs.iter().position(|l| l.label == "User Applications") else {
            assert!(
                !dir_has_visible_entry(&home_dir().join("Applications")),
                "a non-empty ~/Applications must produce a row"
            );
            return;
        };
        assert_eq!(locs[at].path, home_dir().join("Applications"));
        assert_eq!(locs[at - 1].label, "Applications");
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
