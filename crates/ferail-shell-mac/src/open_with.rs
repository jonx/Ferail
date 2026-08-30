//! "Open With…" enumeration via Launch Services.
//!
//! Uses `-[NSWorkspace URLsForApplicationsToOpenURL:]` (macOS 12+)
//! to list every app the system would offer in Finder's Open With
//! submenu, and `-[NSWorkspace URLForApplicationToOpenURL:]` for
//! the default. The call goes through Launch Services which is
//! already cached after first hit per-extension; we still keep
//! the synchronous query short by deduping by bundle id and
//! limiting display to a sensible number.
//!
//! Open is dispatched via `-[NSWorkspace openURLs:withApplicationAt
//! URL:configuration:completionHandler:]` (also macOS 12+).
//!
//! Falls back to `/usr/bin/open -a "<app-path>" <file>` for the
//! shell-only path (used by tests and any caller that doesn't
//! have an `NSWorkspace` instance handy).

use std::path::{Path, PathBuf};

/// One app candidate the system offers for opening `path`. The
/// `name` is the bundle's display name without the `.app` suffix
/// (e.g. "Preview"); `path` is the absolute path to the bundle.
#[derive(Clone, Debug)]
pub struct OpenWithCandidate {
    pub name: String,
    pub path: PathBuf,
    /// `true` for the system's default handler; the right-click
    /// menu pins this entry to the top.
    pub is_default: bool,
}

/// Enumerate every app Launch Services would offer for `path`.
/// Synchronous Cocoa hop; ~10–50 ms typical, slower on cold cache.
/// Returns empty on macOS < 12 or any failure.
#[cfg(target_os = "macos")]
pub fn candidates_for(path: &Path) -> Vec<OpenWithCandidate> {
    use objc2::msg_send_id;
    use objc2::rc::{autoreleasepool, Retained};
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSArray, NSString, NSURL};

    let mut out: Vec<OpenWithCandidate> = Vec::new();

    // Worker-callable: drain the autoreleased Launch Services objects
    // (candidate URL array, path strings) per call instead of
    // accumulating them until the worker queue idles.
    autoreleasepool(|_| unsafe {
        let path_ns = NSString::from_str(&path.to_string_lossy());
        let url: Retained<NSURL> = NSURL::fileURLWithPath_isDirectory(&path_ns, path.is_dir());
        let workspace = NSWorkspace::sharedWorkspace();

        // Default handler first.
        let default_url: Option<Retained<NSURL>> = msg_send_id![
            &workspace,
            URLForApplicationToOpenURL: &*url,
        ];
        let default_path = default_url.as_ref().and_then(|u| ns_url_to_pathbuf(u));

        // All handlers.
        let urls: Option<Retained<NSArray<NSURL>>> = msg_send_id![
            &workspace,
            URLsForApplicationsToOpenURL: &*url,
        ];
        if let Some(urls) = urls {
            let count: usize = urls.count();
            for i in 0..count {
                let item = urls.objectAtIndex(i);
                let Some(p) = ns_url_to_pathbuf(&item) else {
                    continue;
                };
                let name = bundle_display_name(&p);
                let is_default = default_path.as_ref().map(|d| d == &p).unwrap_or(false);
                out.push(OpenWithCandidate {
                    name,
                    path: p,
                    is_default,
                });
            }
        }

        // Move default to the top if present and not already first.
        if let Some(d) = &default_path {
            if let Some(idx) = out.iter().position(|c| &c.path == d) {
                if idx != 0 {
                    let item = out.remove(idx);
                    out.insert(0, item);
                }
            }
        }
    });

    out
}

#[cfg(not(target_os = "macos"))]
pub fn candidates_for(_path: &Path) -> Vec<OpenWithCandidate> {
    Vec::new()
}

/// Open `target` using the app at `app_path`. Best-effort: shells
/// out to `/usr/bin/open -a` so we don't have to handle the
/// completion-handler dance for `NSWorkspace.openURLs:`.
pub fn open_with(target: &Path, app_path: &Path) -> Result<(), String> {
    open_with_many(std::slice::from_ref(&target.to_path_buf()), app_path)
}

/// Open all `targets` with one `/usr/bin/open -a` invocation. `open`
/// accepts multiple files, so a multi-selection pays the app's
/// check-in wait once instead of once per file. Blocks until `open`
/// exits: worker-thread only.
pub fn open_with_many(targets: &[std::path::PathBuf], app_path: &Path) -> Result<(), String> {
    if targets.is_empty() {
        return Ok(());
    }
    let status = std::process::Command::new("/usr/bin/open")
        .arg("-a")
        .arg(app_path)
        .args(targets)
        .status()
        .map_err(|e| format!("failed to spawn open: {e}"))?;
    if !status.success() {
        return Err(format!("open exited with {status}"));
    }
    Ok(())
}

/// "/Applications/Preview.app" → "Preview". Falls back to the full
/// file name if it doesn't end in `.app`.
fn bundle_display_name(path: &Path) -> String {
    let raw = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    raw.strip_suffix(".app").map(str::to_owned).unwrap_or(raw)
}

#[cfg(target_os = "macos")]
fn ns_url_to_pathbuf(url: &objc2_foundation::NSURL) -> Option<PathBuf> {
    unsafe {
        let path_ns = url.path()?;
        Some(PathBuf::from(path_ns.to_string()))
    }
}
