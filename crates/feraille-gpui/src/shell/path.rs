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

/// Canonicalize an external path for identity registration (the
/// ARCHITECTURE.md path-identity contract: paths from outside the app
/// — typed input, persisted state, external drops — are canonicalized
/// once at their entry boundary). Falls back to the input unchanged
/// when canonicalization fails (nonexistent path, permission error):
/// navigation's background enumeration owns the error reporting.
///
/// This STATS THE FILESYSTEM — never call it on a render or input
/// path; run it at init, on the background executor, or inside an
/// already-async load.
pub fn canonicalize_for_identity(path: PathBuf) -> PathBuf {
    // The one sanctioned wrapper around the raw canonicalize the lint
    // bans — every other call site goes through this fn, off-thread.
    #[allow(clippy::disallowed_methods)]
    std::fs::canonicalize(&path).unwrap_or(path)
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
                // Navigation keeps the real (possibly `\\?\`-verbatim) path;
                // the label is the clean display form so a canonicalized root
                // never shows as `\\?\C:\`.
                let label = feraille_fs_native::paths::display_path(&accum);
                out.push((label, accum.clone()));
            }
            Component::Normal(s) => {
                accum.push(s);
                // The label is what the user reads; the accumulated path is
                // what we navigate to. On macOS a folder stored with a `:`
                // shows as `/` (Finder parity) without changing the real path.
                let label =
                    feraille_fs_native::paths::display_leaf(s.to_string_lossy().as_ref())
                        .into_owned();
                out.push((label, accum.clone()));
            }
            Component::CurDir => {}
            Component::ParentDir => {
                accum.pop();
            }
        }
    }

    out
}
