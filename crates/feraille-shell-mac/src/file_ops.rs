//! File operations the right-click menu invokes: Duplicate (copy
//! with " copy" / " copy 2" name resolution, matching Finder) and
//! Make Alias (write a macOS bookmark file Finder will resolve).
//!
//! Both are synchronous on the calling thread — callers dispatch
//! from a worker per [`docs/UI_NONBLOCKING.md`]. Failures return an
//! error string the host can surface as a toast.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Duplicate `src` next to itself, naming the copy " copy",
/// " copy 2", … to mirror Finder. Returns the destination path.
pub fn duplicate(src: &Path) -> Result<PathBuf, String> {
    let parent = src
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", src.display()))?;
    let stem = src
        .file_stem()
        .map(OsStr::to_owned)
        .ok_or_else(|| format!("no file name in {}", src.display()))?;
    let ext = src.extension().map(OsStr::to_owned);

    let dst = pick_suffixed_name(parent, &stem, ext.as_deref(), "copy")
        .ok_or_else(|| "exhausted copy index range".to_string())?;

    if src.is_dir() {
        copy_dir_recursive(src, &dst).map_err(|e| format!("{e}"))?;
    } else {
        std::fs::copy(src, &dst).map_err(|e| format!("{e}"))?;
    }
    Ok(dst)
}

/// Resolve a non-colliding "<stem> <suffix>[N]" path next to a
/// known parent. `n=1` drops the index ("foo copy"); `n>=2` adds
/// it ("foo copy 2"). Returns `None` if 9999 candidates collide.
fn pick_suffixed_name(
    parent: &Path,
    stem: &OsStr,
    ext: Option<&OsStr>,
    suffix: &str,
) -> Option<PathBuf> {
    let stem_str = stem.to_string_lossy();
    for n in 1..=9999 {
        let candidate_stem = if n == 1 {
            format!("{stem_str} {suffix}")
        } else {
            format!("{stem_str} {suffix} {n}")
        };
        let mut candidate = parent.join(&candidate_stem);
        if let Some(e) = ext {
            candidate.set_extension(e);
        }
        if !candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if kind.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if kind.is_symlink() {
            if let Ok(link_target) = std::fs::read_link(&from) {
                #[cfg(unix)]
                std::os::unix::fs::symlink(&link_target, &to)?;
                #[cfg(not(unix))]
                std::fs::copy(&from, &to)?;
                let _ = link_target;
            } else {
                std::fs::copy(&from, &to)?;
            }
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Write a macOS Finder-compatible alias file pointing at `target`.
/// The alias lives at "<target_stem> alias[.ext]" next to the
/// source, matching Finder's naming. Uses `NSURL.bookmarkData(...)`
/// with `NSURLBookmarkCreationSuitableForBookmarkFile` so Finder
/// resolves it as an alias on double-click.
#[cfg(target_os = "macos")]
pub fn make_alias(target: &Path) -> Result<PathBuf, String> {
    use objc2::msg_send;
    use objc2::msg_send_id;
    use objc2::rc::Retained;
    use objc2::runtime::AnyClass;
    use objc2_foundation::{NSData, NSError, NSString, NSURL};

    let parent = target
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", target.display()))?;
    let stem = target
        .file_stem()
        .map(OsStr::to_owned)
        .ok_or_else(|| format!("no file name in {}", target.display()))?;

    // Alias files get no extension; Finder treats them by type metadata.
    let dst = pick_suffixed_name(parent, &stem, None, "alias")
        .ok_or_else(|| "exhausted alias index range".to_string())?;

    // NSURLBookmarkCreationSuitableForBookmarkFile = 1 << 10 (Apple SDK).
    const SUITABLE_FOR_BOOKMARK_FILE: u64 = 1 << 10;

    unsafe {
        let src_path_ns = NSString::from_str(&target.to_string_lossy());
        let src_url: Retained<NSURL> =
            NSURL::fileURLWithPath_isDirectory(&src_path_ns, target.is_dir());

        // -[NSURL bookmarkDataWithOptions:includingResourceValuesForKeys:relativeToURL:error:]
        let mut err: *mut NSError = std::ptr::null_mut();
        let bookmark: Option<Retained<NSData>> = msg_send_id![
            &src_url,
            bookmarkDataWithOptions: SUITABLE_FOR_BOOKMARK_FILE,
            includingResourceValuesForKeys: std::ptr::null::<objc2::runtime::AnyObject>(),
            relativeToURL: std::ptr::null::<NSURL>(),
            error: &mut err,
        ];
        let bookmark = bookmark.ok_or_else(|| ns_error_message(err, "bookmarkData failed"))?;

        let dst_path_ns = NSString::from_str(&dst.to_string_lossy());
        let dst_url: Retained<NSURL> =
            NSURL::fileURLWithPath_isDirectory(&dst_path_ns, false);

        // +[NSURL writeBookmarkData:toURL:options:error:]
        let mut werr: *mut NSError = std::ptr::null_mut();
        let cls: &AnyClass = AnyClass::get("NSURL").ok_or("NSURL class missing")?;
        let ok: bool = msg_send![
            cls,
            writeBookmarkData: &*bookmark,
            toURL: &*dst_url,
            options: SUITABLE_FOR_BOOKMARK_FILE,
            error: &mut werr,
        ];
        if !ok {
            return Err(ns_error_message(werr, "writeBookmarkData failed"));
        }
    }

    Ok(dst)
}

#[cfg(not(target_os = "macos"))]
pub fn make_alias(_target: &Path) -> Result<PathBuf, String> {
    Err("make_alias is macOS-only".into())
}

/// Unmount and eject the volume mounted at `path`. macOS:
/// `-[NSWorkspace unmountAndEjectDeviceAtURL:error:]`, which handles
/// removable media, external disks, and disk images. Synchronous —
/// callers dispatch from a worker. Returns an error string on failure
/// (e.g. a busy volume with open files), suitable for a toast.
#[cfg(target_os = "macos")]
pub fn eject_volume(path: &Path) -> Result<(), String> {
    use objc2::msg_send;
    use objc2::msg_send_id;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSError, NSString, NSURL};

    unsafe {
        let path_ns = NSString::from_str(&path.to_string_lossy());
        let url: Retained<NSURL> = NSURL::fileURLWithPath_isDirectory(&path_ns, true);

        let cls: &AnyClass = AnyClass::get("NSWorkspace").ok_or("NSWorkspace class missing")?;
        let workspace: Retained<AnyObject> = msg_send_id![cls, sharedWorkspace];

        // -[NSWorkspace unmountAndEjectDeviceAtURL:error:]
        let mut err: *mut NSError = std::ptr::null_mut();
        let ok: bool = msg_send![
            &*workspace,
            unmountAndEjectDeviceAtURL: &*url,
            error: &mut err,
        ];
        if !ok {
            return Err(ns_error_message(err, "eject failed"));
        }
    }
    Ok(())
}

/// Best-effort string from an `NSError*` (which may be null). Falls
/// back to the static fallback when the error is missing or its
/// `localizedDescription` selector isn't reachable.
#[cfg(target_os = "macos")]
pub(crate) unsafe fn ns_error_message(err: *mut objc2_foundation::NSError, fallback: &str) -> String {
    use objc2::msg_send;
    use objc2_foundation::NSString;
    if err.is_null() {
        return fallback.to_string();
    }
    let desc: *mut NSString = msg_send![&*err, localizedDescription];
    if desc.is_null() {
        return fallback.to_string();
    }
    (&*desc).to_string()
}
