//! Finder colour tags. Reads and writes via `URLTagNamesKey`:
//! the public Cocoa API that round-trips through Launch Services
//! so a tag we set here shows up with the right colour in Finder
//! and vice-versa.
//!
//! Both calls are synchronous Cocoa hops (no I/O beyond the
//! single resource-value get/set), so they're fast enough for the
//! UI thread on a small selection. For larger selections, callers
//! still dispatch from a worker per the nonblocking contract.

use std::path::Path;

use ferail_core::commands::TagColor;

/// Read the tags currently set on `path`. Empty vec on no tags
/// or read failure (the latter is best-effort: we'd rather show
/// "no tags" than fail the whole right-click). Returns the raw
/// tag names so callers can distinguish a user's custom tag from
/// one of our seven canonical colours.
#[cfg(target_os = "macos")]
pub fn read_tags(path: &Path) -> Vec<String> {
    use objc2::msg_send;
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSArray, NSError, NSString, NSURL};

    // Worker-callable: drain the autoreleased Cocoa objects (NSError,
    // the returned tag array) per call: a large tag sweep otherwise
    // accumulates them until the worker queue idles.
    autoreleasepool(|_| unsafe {
        let path_ns = NSString::from_str(&path.to_string_lossy());
        let url: Retained<NSURL> = NSURL::fileURLWithPath_isDirectory(&path_ns, path.is_dir());

        let key = NSString::from_str("NSURLTagNamesKey");
        let mut value: *mut AnyObject = std::ptr::null_mut();
        let mut err: *mut NSError = std::ptr::null_mut();
        let ok: bool = msg_send![
            &url,
            getResourceValue: &mut value,
            forKey: &*key,
            error: &mut err,
        ];
        if !ok || value.is_null() {
            return Vec::new();
        }
        // Returned object is an NSArray<NSString>* (or nil if no tags).
        let array_ptr: *mut NSArray<NSString> = value as *mut _;
        if array_ptr.is_null() {
            return Vec::new();
        }
        let array: &NSArray<NSString> = &*array_ptr;
        let count: usize = array.count();
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let s = array.objectAtIndex(i);
            out.push(s.to_string());
        }
        out
    })
}

#[cfg(not(target_os = "macos"))]
pub fn read_tags(_path: &Path) -> Vec<String> {
    Vec::new()
}

/// Subset of `read_tags` filtered to the seven canonical colours.
/// User-defined tag names that happen to match a colour name still
/// register; everything else is dropped.
pub fn read_canonical_tags(path: &Path) -> Vec<TagColor> {
    read_tags(path)
        .iter()
        .filter_map(|n| TagColor::from_name(n))
        .collect()
}

/// Write `names` as the full tag list on `path`, replacing
/// whatever was there. Pass an empty slice to clear all tags.
/// Returns `Err` with `localizedDescription` on Cocoa failure.
#[cfg(target_os = "macos")]
pub fn write_tags(path: &Path, names: &[&str]) -> Result<(), String> {
    use objc2::msg_send;
    use objc2::rc::{autoreleasepool, Retained};
    use objc2_foundation::{NSArray, NSError, NSString, NSURL};

    // Worker-callable: see read_tags.
    autoreleasepool(|_| unsafe {
        let path_ns = NSString::from_str(&path.to_string_lossy());
        let url: Retained<NSURL> = NSURL::fileURLWithPath_isDirectory(&path_ns, path.is_dir());

        // NSString has an NSMutableString subclass so it's not
        // `IsRetainable`: `NSArray::from_vec` takes ownership of
        // `Vec<Retained<T>>` and works around that.
        let strings: Vec<Retained<NSString>> =
            names.iter().map(|n| NSString::from_str(n)).collect();
        let array: Retained<NSArray<NSString>> = NSArray::from_vec(strings);

        let key = NSString::from_str("NSURLTagNamesKey");
        let mut err: *mut NSError = std::ptr::null_mut();
        let ok: bool = msg_send![
            &url,
            setResourceValue: &*array,
            forKey: &*key,
            error: &mut err,
        ];
        if !ok {
            return Err(crate::file_ops::ns_error_message(
                err,
                "setResourceValue failed",
            ));
        }
        Ok(())
    })
}

#[cfg(not(target_os = "macos"))]
pub fn write_tags(_path: &Path, _names: &[&str]) -> Result<(), String> {
    Err("write_tags is macOS-only".into())
}

/// Toggle a single colour tag on `path`. If it's already set,
/// remove it; otherwise add it. Other tags (including user-defined
/// ones) are preserved.
#[cfg(target_os = "macos")]
pub fn toggle_tag(path: &Path, color: TagColor) -> Result<(), String> {
    let mut current = read_tags(path);
    let target = color.name();
    if let Some(pos) = current.iter().position(|n| n == target) {
        current.remove(pos);
    } else {
        current.push(target.to_string());
    }
    let refs: Vec<&str> = current.iter().map(String::as_str).collect();
    write_tags(path, &refs)
}

#[cfg(not(target_os = "macos"))]
pub fn toggle_tag(_path: &Path, _color: TagColor) -> Result<(), String> {
    Err("toggle_tag is macOS-only".into())
}
