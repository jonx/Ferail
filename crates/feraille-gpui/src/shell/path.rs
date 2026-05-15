use std::ffi::OsString;
use std::path::{Path, PathBuf};

use feraille_fs_native::home_dir;

/// Parse a user-typed breadcrumb-input string into a real path:
/// expands a leading `~` to `$HOME`. It deliberately does not
/// canonicalise or stat the path on the UI thread; navigation's
/// background enumeration reports errors.
///
/// Strips both `/` and `\` after the tilde so `~/Documents` and
/// `~\Documents` both work on Windows.
pub fn parse_breadcrumb_path(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix('~') {
        let mut h = home_dir();
        let suffix = rest.trim_start_matches(['/', '\\']);
        if !suffix.is_empty() {
            h.push(suffix);
        }
        h
    } else {
        PathBuf::from(trimmed)
    }
}

/// Split `path` into clickable breadcrumb segments. Each entry is
/// `(visible_label, path_to_navigate_to_on_click)`. The first entry
/// represents the filesystem root — on macOS/Linux always `/`; on
/// Windows the drive root (e.g. `C:\`) when the path carries a
/// `Prefix` component, or `\` for current-drive-relative paths.
///
/// Public for the integration test in `tests/path_segments.rs` —
/// keeping it private and using an inline `#[cfg(test)] mod tests`
/// crashes the compiler (gpui's type graph plus the macro recursion
/// from `#[test]` overflows syn's parser).
pub fn path_segments(path: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut accum = PathBuf::new();
    let mut pending_prefix: Option<OsString> = None;

    for component in path.components() {
        use std::path::Component;
        match component {
            Component::Prefix(p) => {
                pending_prefix = Some(p.as_os_str().to_os_string());
            }
            Component::RootDir => {
                let mut root_str: OsString = pending_prefix.take().unwrap_or_default();
                root_str.push(std::path::MAIN_SEPARATOR_STR);
                accum = PathBuf::from(root_str);
                let label = accum.to_string_lossy().into_owned();
                out.push((label, accum.clone()));
            }
            Component::Normal(s) => {
                accum.push(s);
                out.push((s.to_string_lossy().into_owned(), accum.clone()));
            }
            Component::CurDir => {}
            Component::ParentDir => {
                accum.pop();
            }
        }
    }

    out
}
