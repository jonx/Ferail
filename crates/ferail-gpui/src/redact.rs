//! Privacy redaction for the diagnostics & issue-report surfaces.
//!
//! Ferail's diagnostics bundle, the "Copy report" action, and the in-app
//! activity-trail view all describe *what the user did*, including the folders
//! they browsed and the files they opened. Those paths are the one genuinely
//! sensitive thing a bug report could leak, so the app must be able to promise,
//! truthfully, that a shared report contains **no file or folder names**.
//!
//! Two layers compose:
//! - **Account scrub** ([`crate::report::redact_username`]) is always applied
//!   and removes the home-directory prefix, i.e. the account name.
//! - **Path redaction** (this module: a user toggle that defaults to *on*)
//!   goes further: every filesystem path is reduced to its *shape*: the root
//!   anchor, the depth, and the final file extension, with each name replaced
//!   by `…`. `/Users/ada/Taxes/2025/return.pdf` becomes `/…/…/…/…/….pdf`: still
//!   useful for reproducing a bug ("five levels deep, opening a PDF") but
//!   revealing nothing about the user.
//!
//! The toggle is a process-global `AtomicBool` (mirroring the `obs` log
//! threshold) so the pure report/trail code can consult it without a GPUI
//! context. It starts *on* so a fresh install never emits a file name in a
//! report until the user deliberately opts out.

use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, Ordering};

/// Path redaction defaults to **on**.
static ENABLED: AtomicBool = AtomicBool::new(true);

/// Whether reports redact file names and paths. Default `true`.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Set the redaction state (from the persisted preference at startup, or the
/// Settings → Diagnostics toggle at runtime).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Reduce a filesystem path to a name-free shape: the root anchor, one `…` per
/// path segment, and the final file extension. This *always* redacts,
/// regardless of [`enabled`]: callers decide whether to invoke it.
///
/// Examples (POSIX): `/a/b/c.pdf` → `/…/…/….pdf`, `/Volumes/Disk` → `/…/…`,
/// `/` → `/`. Windows drive and UNC anchors are preserved with `\` separators.
pub fn redact_path(path: &Path) -> String {
    let mut anchor = String::new();
    let mut windows_style = false;
    let mut depth = 0usize;
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => {
                anchor = p.as_os_str().to_string_lossy().into_owned();
                windows_style = true;
            }
            Component::RootDir => {
                if anchor.is_empty() {
                    anchor.push('/');
                } else {
                    anchor.push('\\');
                    windows_style = true;
                }
            }
            Component::CurDir => {}
            // A name (or `..`) counts toward depth but contributes no text.
            Component::Normal(_) | Component::ParentDir => depth += 1,
        }
    }
    let sep = if windows_style { '\\' } else { '/' };
    // Preserve the final extension: the file *type* is useful for a bug
    // report; the name is not.
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty())
        .map(|e| e.to_ascii_lowercase());

    if depth == 0 {
        return if anchor.is_empty() {
            "…".to_string()
        } else {
            anchor
        };
    }

    let mut out = anchor;
    if !out.is_empty() && !out.ends_with(sep) {
        out.push(sep);
    }
    for i in 0..depth {
        if i > 0 {
            out.push(sep);
        }
        out.push('…');
        if i + 1 == depth {
            if let Some(ext) = &ext {
                out.push('.');
                out.push_str(ext);
            }
        }
    }
    out
}

/// Best-effort redaction of filesystem paths embedded in free-form text (e.g. a
/// user's note or an error message). A no-op when [`enabled`] is false.
///
/// Only recognises whole-path *tokens* (a token that starts with `/`, `~/`, a
/// Windows drive, or a UNC prefix); it does not hunt for paths with embedded
/// spaces inside prose. The structured surfaces, the activity trail, redact
/// those exactly via [`redact_path`], so this is a backstop for note text.
pub fn scrub_text(text: &str) -> String {
    if !enabled() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            flush_token(&mut token, &mut out);
            out.push(ch);
        } else {
            token.push(ch);
        }
    }
    flush_token(&mut token, &mut out);
    out
}

fn flush_token(token: &mut String, out: &mut String) {
    if token.is_empty() {
        return;
    }
    if looks_like_path(token) {
        out.push_str(&redact_path(Path::new(token.as_str())));
    } else {
        out.push_str(token);
    }
    token.clear();
}

/// Conservative test: does this whitespace-delimited token look like a
/// filesystem path? Tuned to never reshape an ordinary word.
fn looks_like_path(tok: &str) -> bool {
    // Unix absolute with at least one more separator: /a/b
    if let Some(rest) = tok.strip_prefix('/') {
        if rest.contains('/') {
            return true;
        }
    }
    // Home-relative: ~/a
    if tok.starts_with("~/") {
        return true;
    }
    // UNC: \\server\share
    if tok.starts_with("\\\\") {
        return true;
    }
    // Windows drive: C:\ or C:/
    let b = tok.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_to_name_free_shape() {
        assert_eq!(
            redact_path(Path::new("/Users/ada/Taxes/2025/return.pdf")),
            "/…/…/…/…/….pdf"
        );
        // A folder (no extension) keeps depth but no trailing type.
        assert_eq!(redact_path(Path::new("/Volumes/Backup/Photos")), "/…/…/…");
        // The root itself has nothing to hide.
        assert_eq!(redact_path(Path::new("/")), "/");
        // Relative paths get no leading anchor.
        assert_eq!(redact_path(Path::new("docs/readme.md")), "…/….md");
    }

    #[test]
    fn redacted_path_leaks_no_original_name() {
        let secret = Path::new("/Users/ada/Private/very-secret-merger.docx");
        let shape = redact_path(secret);
        for name in ["ada", "Private", "very-secret-merger"] {
            assert!(!shape.contains(name), "leaked {name:?} in {shape:?}");
        }
        // The file type survives: useful, not sensitive.
        assert!(shape.ends_with(".docx"));
    }

    #[test]
    fn scrub_text_toggles_with_global_state() {
        // Runs the on/off assertions sequentially in one test so the shared
        // global can't race a sibling test.
        let prose = "failed to open /Users/ada/Secret/plan.key while saving";
        set_enabled(true);
        let scrubbed = scrub_text(prose);
        assert!(!scrubbed.contains("Secret"), "{scrubbed}");
        assert!(scrubbed.contains("failed to open"));
        assert!(scrubbed.contains(".key"));

        set_enabled(false);
        assert_eq!(scrub_text(prose), prose);

        // Restore the default so other tests in this binary see redaction on.
        set_enabled(true);
    }

    #[test]
    fn scrub_text_leaves_ordinary_words_alone() {
        set_enabled(true);
        let prose = "the app/ui crashed when I clicked save:now";
        // No token is a real path, so nothing is reshaped.
        assert_eq!(scrub_text(prose), prose);
    }
}
