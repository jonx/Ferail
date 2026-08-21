//! Pure-domain tests for the Go to Folder (Cmd+G) input parser.
//!
//! Integration test rather than `#[cfg(test)] mod` for the same reason
//! as `path_segments.rs`: the inline form overflows syn's parser inside
//! this crate.

// Every test below is `cfg(unix)` (POSIX path spellings; the resolve tests
// compare against `canonicalize_for_identity`, whose Windows answer can
// carry a `\\?\` prefix). Gate the imports the same way so the Windows
// clippy leg — which compiles this file under `-D warnings` — doesn't see
// them as unused.
#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
use ferail_gpui::shell::{parse_pasted_path, resolve_go_to_target};

#[cfg(unix)]
#[test]
fn plain_path_passes_through() {
    assert_eq!(
        parse_pasted_path("/Users/alice/Source"),
        PathBuf::from("/Users/alice/Source")
    );
}

#[cfg(unix)]
#[test]
fn surrounding_whitespace_and_newline_are_trimmed() {
    assert_eq!(
        parse_pasted_path("  /Users/alice/Source\n"),
        PathBuf::from("/Users/alice/Source")
    );
}

#[cfg(unix)]
#[test]
fn surrounding_quotes_are_stripped() {
    assert_eq!(
        parse_pasted_path("\"/Users/alice/My Folder\""),
        PathBuf::from("/Users/alice/My Folder")
    );
    assert_eq!(
        parse_pasted_path("'/Users/alice/My Folder'"),
        PathBuf::from("/Users/alice/My Folder")
    );
}

#[cfg(unix)]
#[test]
fn an_unmatched_quote_is_left_alone() {
    // A directory really named `"weird` keeps its leading quote.
    assert_eq!(
        parse_pasted_path("/Users/alice/\"weird"),
        PathBuf::from("/Users/alice/\"weird")
    );
}

#[cfg(unix)]
#[test]
fn terminal_escaped_spaces_are_unescaped() {
    assert_eq!(
        parse_pasted_path("/Users/alice/My\\ Folder"),
        PathBuf::from("/Users/alice/My Folder")
    );
}

#[cfg(unix)]
#[test]
fn file_url_is_decoded() {
    assert_eq!(
        parse_pasted_path("file:///Users/alice/My%20Folder"),
        PathBuf::from("/Users/alice/My Folder")
    );
    assert_eq!(
        parse_pasted_path("file://localhost/Users/alice/Source"),
        PathBuf::from("/Users/alice/Source")
    );
}

#[cfg(unix)]
#[test]
fn percent_that_is_not_an_escape_survives() {
    assert_eq!(
        parse_pasted_path("file:///Users/alice/100%25/a%zz"),
        PathBuf::from("/Users/alice/100%/a%zz")
    );
}

#[cfg(unix)]
#[test]
fn remote_file_url_is_not_reinterpreted_as_local() {
    // `file://server/share` names a host, not `/share`. Left untouched
    // so navigation reports it rather than opening the wrong folder.
    assert_eq!(
        parse_pasted_path("file://server/share"),
        PathBuf::from("file://server/share")
    );
}

#[cfg(unix)]
#[test]
fn tilde_expands() {
    let home = std::env::var("HOME").expect("HOME set");
    assert_eq!(
        parse_pasted_path("~/Documents"),
        PathBuf::from(format!("{home}/Documents"))
    );
}

#[cfg(unix)]
#[test]
fn resolve_maps_a_file_to_its_folder() {
    let dir = std::env::temp_dir();
    let file = dir.join("ferail-go-to-folder-test.txt");
    std::fs::write(&file, b"x").expect("write temp file");
    let resolved = resolve_go_to_target(file.clone());
    let _ = std::fs::remove_file(&file);
    assert_eq!(resolved, ferail_gpui::shell::canonicalize_for_identity(dir));
}

#[cfg(unix)]
#[test]
fn resolve_keeps_a_directory() {
    let dir = std::env::temp_dir();
    assert_eq!(
        resolve_go_to_target(dir.clone()),
        ferail_gpui::shell::canonicalize_for_identity(dir)
    );
}

#[cfg(unix)]
#[test]
fn resolve_keeps_a_path_that_does_not_exist() {
    let missing = PathBuf::from("/nonexistent-ferail/deeper/still");
    assert_eq!(resolve_go_to_target(missing.clone()), missing);
}
