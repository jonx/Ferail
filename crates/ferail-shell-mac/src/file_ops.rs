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
    let parent = target
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", target.display()))?;
    make_alias_in(target, parent)
}

/// Like [`make_alias`] but writes the alias into `dest_dir` instead of
/// next to `target` — used by Cmd+Option alias-drop, where the alias
/// belongs in the folder it was dropped on.
#[cfg(target_os = "macos")]
pub fn make_alias_in(target: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    use objc2::msg_send;
    use objc2::msg_send_id;
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::AnyClass;
    use objc2_foundation::{NSData, NSError, NSString, NSURL};

    let stem = target
        .file_stem()
        .map(OsStr::to_owned)
        .ok_or_else(|| format!("no file name in {}", target.display()))?;

    // Alias files get no extension; Finder treats them by type metadata.
    let dst = pick_suffixed_name(dest_dir, &stem, None, "alias")
        .ok_or_else(|| "exhausted alias index range".to_string())?;

    // NSURLBookmarkCreationSuitableForBookmarkFile = 1 << 10 (Apple SDK).
    const SUITABLE_FOR_BOOKMARK_FILE: u64 = 1 << 10;

    // Worker-callable: drain the autoreleased Cocoa objects (bookmark
    // NSData, NSErrors) per call instead of accumulating them until the
    // worker queue idles.
    autoreleasepool(|_| unsafe {
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

        Ok(dst)
    })
}

#[cfg(not(target_os = "macos"))]
pub fn make_alias(_target: &Path) -> Result<PathBuf, String> {
    Err("make_alias is macOS-only".into())
}

#[cfg(not(target_os = "macos"))]
pub fn make_alias_in(_target: &Path, _dest_dir: &Path) -> Result<PathBuf, String> {
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
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSError, NSString, NSURL};

    // Worker-callable: see make_alias_in.
    autoreleasepool(|_| unsafe {
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
        Ok(())
    })
}

/// Unmount **every** volume on the physical device backing
/// `volume_paths[0]`, then eject the device — Finder's "Eject All".
/// macOS: `-[NSFileManager unmountVolumeAtURL:options:completionHandler:]`
/// with `NSFileManagerUnmountAllPartitionsAndEjectDisk`, the one
/// primitive that handles sibling partitions atomically. (Looping
/// `unmountAndEjectDeviceAtURL` per volume does NOT work: it returns
/// OSStatus -36 for every partition except the last one still mounted,
/// verified on-device.) The extra paths are unused on macOS — the
/// option ejects the whole device from any one of its volumes — but the
/// slice keeps the cross-platform signature, since Windows/Linux
/// dismount each volume individually.
///
/// Synchronous — blocks on the completion handler (which fires on a
/// dispatch queue, not the caller's thread), so callers dispatch from
/// a worker. A generous timeout guards against a completion that never
/// arrives; the flush of a slow device can take a while, so it errs
/// long.
#[cfg(target_os = "macos")]
pub fn eject_device(volume_paths: &[&Path]) -> Result<(), String> {
    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::msg_send_id;
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSError, NSString, NSURL};

    let Some(first) = volume_paths.first() else {
        return Err("eject: no volumes given".into());
    };

    // NSFileManagerUnmountOptions (NSFileManager.h).
    const UNMOUNT_ALL_PARTITIONS_AND_EJECT_DISK: usize = 1 << 0;
    const UNMOUNT_WITHOUT_UI: usize = 1 << 1;

    autoreleasepool(|_| unsafe {
        let path_ns = NSString::from_str(&first.to_string_lossy());
        let url: Retained<NSURL> = NSURL::fileURLWithPath_isDirectory(&path_ns, true);

        let cls: &AnyClass =
            AnyClass::get("NSFileManager").ok_or("NSFileManager class missing")?;
        let fm: Retained<AnyObject> = msg_send_id![cls, defaultManager];

        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let block = RcBlock::new(move |err: *mut NSError| {
            let result = if err.is_null() {
                Ok(())
            } else {
                Err(ns_error_message(err, "eject failed"))
            };
            let _ = tx.send(result);
        });

        let options = UNMOUNT_ALL_PARTITIONS_AND_EJECT_DISK | UNMOUNT_WITHOUT_UI;
        let _: () = msg_send![
            &*fm,
            unmountVolumeAtURL: &*url,
            options: options,
            completionHandler: &*block,
        ];

        match rx.recv_timeout(std::time::Duration::from_secs(180)) {
            Ok(result) => result,
            Err(_) => Err("eject timed out".into()),
        }
    })
}

/// Names of processes holding files open on the volume mounted at
/// `path` — the "why won't it eject" answer for a failed eject.
///
/// Uses libproc's `proc_listpidspath` (public since 10.5; the same
/// machinery `lsof -- /Volumes/X` rides), so no subprocess and no
/// directory walk. `PATH_IS_VOLUME` matches any open file on the
/// volume; `EXCLUDE_EVTONLY` skips event-only watchers (Spotlight/FSEvents
/// kqueue fds) that don't actually block an unmount, so we don't accuse
/// innocent daemons. Best-effort: empty on any failure. Sorted, deduped,
/// capped at 5. Synchronous — callers run this on a worker.
#[cfg(target_os = "macos")]
pub fn volume_busy_processes(path: &Path) -> Vec<ferail_core::BusyApp> {
    use std::os::raw::{c_char, c_int, c_void};
    use std::os::unix::ffi::OsStrExt;

    const PROC_ALL_PIDS: u32 = 1;
    const PROC_LISTPIDSPATH_PATH_IS_VOLUME: u32 = 1;
    const PROC_LISTPIDSPATH_EXCLUDE_EVTONLY: u32 = 2;

    extern "C" {
        // libproc.h — exported by libSystem, no extra link flags needed.
        fn proc_listpidspath(
            r#type: u32,
            typeinfo: u32,
            path: *const c_char,
            pathflags: u32,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
        fn proc_name(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;
    }

    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return Vec::new();
    };
    let flags = PROC_LISTPIDSPATH_PATH_IS_VOLUME | PROC_LISTPIDSPATH_EXCLUDE_EVTONLY;
    unsafe {
        // First call sizes the pid buffer; second fills it. Pad the
        // buffer so processes appearing in between still fit.
        let bytes = proc_listpidspath(
            PROC_ALL_PIDS,
            0,
            c_path.as_ptr(),
            flags,
            std::ptr::null_mut(),
            0,
        );
        if bytes <= 0 {
            return Vec::new();
        }
        let mut pids = vec![0 as c_int; bytes as usize / 4 + 16];
        let bytes = proc_listpidspath(
            PROC_ALL_PIDS,
            0,
            c_path.as_ptr(),
            flags,
            pids.as_mut_ptr() as *mut c_void,
            (pids.len() * 4) as c_int,
        );
        if bytes <= 0 {
            return Vec::new();
        }
        let count = (bytes as usize / 4).min(pids.len());
        let mut apps: Vec<ferail_core::BusyApp> = Vec::new();
        for &pid in &pids[..count] {
            if pid <= 0 {
                continue;
            }
            let mut buf = [0u8; 256];
            let len = proc_name(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32);
            if len > 0 {
                let name = String::from_utf8_lossy(&buf[..len as usize]).into_owned();
                if !name.is_empty() {
                    apps.push(ferail_core::BusyApp { pid, name });
                }
            }
        }
        // One chip per app name; the kept pid is enough to activate it.
        apps.sort_by(|a, b| a.name.cmp(&b.name));
        apps.dedup_by(|a, b| a.name == b.name);
        apps.truncate(5);
        apps
    }
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
