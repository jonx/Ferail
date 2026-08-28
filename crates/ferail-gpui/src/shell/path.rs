use std::ffi::OsString;
use std::path::{Path, PathBuf};

use ferail_fs_native::home_dir;

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

/// Parse a *pasted* path — the Go to Folder (Cmd+G) prompt's input.
/// Same contract as [`parse_breadcrumb_path`] (no stat, no
/// canonicalisation on the UI thread) but tolerant of the shapes a
/// path arrives in when it comes off the clipboard rather than the
/// keyboard:
///
/// - surrounding single/double quotes (`"…"` from a copied command
///   line, `'…'` from a shell prompt),
/// - a `file://` URL with percent-escapes (what a browser, a chat
///   client, or "Copy as Pathname" from some apps hands over),
/// - backslash-escaped spaces (unix only — the form a Finder drag
///   into Terminal produces; on Windows `\` is the separator so it is
///   left alone),
/// - a leading `~`, via [`parse_breadcrumb_path`].
///
/// Anything it doesn't recognise passes through untouched, so a plain
/// typed path behaves exactly as it does in the breadcrumb.
pub fn parse_pasted_path(raw: &str) -> PathBuf {
    let mut s = raw.trim();
    // A pasted line can carry a trailing newline plus the quotes some
    // sources wrap paths in. Strip one matching pair.
    for quote in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(quote) && s.ends_with(quote) {
            s = &s[1..s.len() - 1];
            break;
        }
    }
    let s = s.trim();
    if let Some(rest) = strip_file_url(s) {
        return PathBuf::from(rest);
    }
    // Terminal-style escapes: `/Users/me/My\ Folder`. Only unix — a
    // Windows path is full of meaningful backslashes.
    #[cfg(unix)]
    let unescaped = s.replace("\\ ", " ");
    #[cfg(not(unix))]
    let unescaped = s.to_string();
    parse_breadcrumb_path(&unescaped)
}

/// `file:///Users/me/My%20Folder` → `/Users/me/My Folder`. Returns
/// `None` when `s` isn't a `file://` URL. Only the local-host forms
/// (`file://` + empty or `localhost` authority) are accepted; a
/// remote authority (`file://server/share`) is left to the caller's
/// untouched fallback rather than silently reinterpreted as local.
fn strip_file_url(s: &str) -> Option<String> {
    let rest = s
        .strip_prefix("file://localhost/")
        .or_else(|| s.strip_prefix("file:///"))?;
    let decoded = percent_decode(rest);
    // `file:///C:/Users/me` carries the drive letter in the first
    // segment — that IS the root, so it keeps no leading slash. Every
    // other form is an absolute unix path whose leading `/` the
    // prefix strip consumed.
    let mut chars = decoded.chars();
    let drive_rooted =
        matches!((chars.next(), chars.next()), (Some(c), Some(':')) if c.is_ascii_alphabetic());
    Some(if drive_rooted {
        decoded
    } else {
        format!("/{decoded}")
    })
}

/// Minimal `%XX` decoder for `file://` URLs. Invalid escapes (a lone
/// `%`, non-hex digits) pass through literally — a path that really
/// contains a `%` still resolves. Decoded bytes are reassembled as
/// UTF-8; a non-UTF-8 sequence falls back to the undecoded input
/// rather than producing replacement characters.
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
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

/// Resolve a Go to Folder (Cmd+G) entry to the directory to open:
/// canonicalize for identity, then fall back to the enclosing folder
/// when the path names a file. A path that resolves to nothing comes
/// back unchanged — navigation's enumeration reports the failure in
/// the pane, the same as a bad breadcrumb entry.
///
/// This STATS THE FILESYSTEM (canonicalize + one `is_dir`) — call it
/// only from a background task, never from a handler.
pub fn resolve_go_to_target(typed: PathBuf) -> PathBuf {
    let canonical = canonicalize_for_identity(typed);
    if canonical.is_dir() {
        return canonical;
    }
    // A file (or a dangling path): open the enclosing folder when
    // there is one that exists, else hand the original back so the
    // pane surfaces the error against what the user actually typed.
    match canonical.parent() {
        Some(parent) if parent.is_dir() => parent.to_path_buf(),
        _ => canonical,
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
                // Navigation keeps the real (possibly `\\?\`-verbatim) path;
                // the label is the clean display form so a canonicalized root
                // never shows as `\\?\C:\`.
                let label = crate::private_mode::present_path(&accum);
                out.push((label, accum.clone()));
            }
            Component::Normal(s) => {
                accum.push(s);
                // The label is what the user reads; the accumulated path is
                // what we navigate to. On macOS a folder stored with a `:`
                // shows as `/` (Finder parity) without changing the real path.
                let raw = ferail_fs_native::paths::display_leaf(s.to_string_lossy().as_ref())
                    .into_owned();
                let label = crate::private_mode::present_leaf_str(&raw, true);
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
