//! AppKit/Cocoa facts for the Get Info panel that have no POSIX equivalent:
//! the uniform type identifier, Finder's localized "Kind" string, the
//! date-added-to-folder, and the Finder attribute bits exposed as NSURL
//! resource keys (hidden extension, package/bundle, alias file).
//!
//! One batched `resourceValuesForKeys:` hop, mirroring the volume lookup in
//! `feraille-fs-native`. Synchronous Cocoa — callers dispatch it from a
//! worker per the nonblocking contract. All fields are `None` off macOS or
//! when a key is unavailable for the path.

use std::path::Path;

/// Cocoa-sourced facts for one path. Each field is `None` when the key
/// wasn't readable (missing, unsupported FS, or non-macOS).
#[derive(Clone, Debug, Default)]
pub struct ShellInfo {
    /// Uniform type identifier, e.g. "public.folder", "public.png".
    pub uti: Option<String>,
    /// Finder's localized type description, e.g. "Folder", "PNG image".
    pub kind: Option<String>,
    /// When the item was added to its containing folder (unix seconds).
    pub added_unix: Option<i64>,
    /// Finder "Hide extension" state.
    pub hidden_extension: Option<bool>,
    /// True for `.app`/`.bundle`-style packages (Finder's "bundle bit").
    pub is_package: Option<bool>,
    /// True for Finder alias files.
    pub is_alias: Option<bool>,
}

#[cfg(target_os = "macos")]
pub fn read_shell_info(path: &Path) -> ShellInfo {
    use objc2::msg_send;
    use objc2::msg_send_id;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::ClassType;
    use objc2_foundation::{
        NSArray, NSString, NSURLAddedToDirectoryDateKey, NSURLHasHiddenExtensionKey,
        NSURLIsAliasFileKey, NSURLIsPackageKey, NSURLLocalizedTypeDescriptionKey,
        NSURLResourceKey, NSURLTypeIdentifierKey, NSURL,
    };

    let Some(path_str) = path.to_str() else {
        return ShellInfo::default();
    };

    unsafe {
        let ns_path = NSString::from_str(path_str);
        let url: Retained<NSURL> = NSURL::fileURLWithPath_isDirectory(&ns_path, path.is_dir());

        // Same `arrayWithObjects:count:` construction the volume lookup uses —
        // the typed `from_slice` ctor wants `IsRetainable`, which NSString's
        // mutable subclass breaks; Apple's key constants are immortal statics
        // so passing raw pointers is sound.
        let key_ptrs: [*const NSURLResourceKey; 6] = [
            NSURLTypeIdentifierKey,
            NSURLLocalizedTypeDescriptionKey,
            NSURLAddedToDirectoryDateKey,
            NSURLHasHiddenExtensionKey,
            NSURLIsPackageKey,
            NSURLIsAliasFileKey,
        ];
        let keys: Retained<NSArray<NSURLResourceKey>> = msg_send_id![
            NSArray::<NSURLResourceKey>::class(),
            arrayWithObjects: key_ptrs.as_ptr(),
            count: key_ptrs.len(),
        ];
        let Ok(dict) = url.resourceValuesForKeys_error(&keys) else {
            return ShellInfo::default();
        };

        let lookup_string = |key: &NSURLResourceKey| -> Option<String> {
            let obj: &AnyObject = dict.get(key)?;
            let ns: &NSString = &*(obj as *const AnyObject as *const NSString);
            Some(ns.to_string())
        };
        let lookup_bool = |key: &NSURLResourceKey| -> Option<bool> {
            let obj: &AnyObject = dict.get(key)?;
            let b: bool = msg_send![obj, boolValue];
            Some(b)
        };
        // NSDate → unix seconds via timeIntervalSince1970 (f64).
        let lookup_date = |key: &NSURLResourceKey| -> Option<i64> {
            let obj: &AnyObject = dict.get(key)?;
            let secs: f64 = msg_send![obj, timeIntervalSince1970];
            Some(secs as i64)
        };

        ShellInfo {
            uti: lookup_string(NSURLTypeIdentifierKey),
            kind: lookup_string(NSURLLocalizedTypeDescriptionKey),
            added_unix: lookup_date(NSURLAddedToDirectoryDateKey),
            hidden_extension: lookup_bool(NSURLHasHiddenExtensionKey),
            is_package: lookup_bool(NSURLIsPackageKey),
            is_alias: lookup_bool(NSURLIsAliasFileKey),
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn read_shell_info(_path: &Path) -> ShellInfo {
    ShellInfo::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_kind_for_home() {
        // /Applications is a real folder on every macOS box; Finder calls it
        // a "Folder" and its UTI is public.folder. Don't hard-assert the
        // localized string (locale-dependent) — just that we got *something*.
        let info = read_shell_info(Path::new("/Applications"));
        assert_eq!(info.uti.as_deref(), Some("public.folder"));
        assert!(info.kind.is_some(), "localized kind present");
        assert_eq!(info.is_package, Some(false));
    }
}
