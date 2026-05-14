use std::path::{Path, PathBuf};

use feraille_fs_native::home_dir;

/// Parse a user-typed breadcrumb-input string into a real path:
/// expands a leading `~` to `$HOME`. It deliberately does not
/// canonicalise or stat the path on the UI thread; navigation's
/// background enumeration reports errors.
pub fn parse_breadcrumb_path(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix('~') {
        let mut h = home_dir();
        let suffix = rest.trim_start_matches('/');
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
/// represents the filesystem root.
///
/// Public for the integration test in `tests/path_segments.rs` —
/// keeping it private and using an inline `#[cfg(test)] mod tests`
/// crashes the compiler (gpui's type graph plus the macro recursion
/// from `#[test]` overflows syn's parser).
pub fn path_segments(path: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut accum = PathBuf::from("/");
    out.push(("/".to_string(), accum.clone()));
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::RootDir => {}
            Component::Normal(s) => {
                accum.push(s);
                out.push((s.to_string_lossy().into_owned(), accum.clone()));
            }
            Component::CurDir => {}
            Component::ParentDir => {
                accum.pop();
            }
            Component::Prefix(_) => {}
        }
    }
    out
}
