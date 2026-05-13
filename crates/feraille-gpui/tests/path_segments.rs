//! Pure-domain test for the breadcrumb path-splitter.
//!
//! Lives as an integration test (not `#[cfg(test)] mod`) because the
//! inline form crashes the compiler — gpui's type graph plus the
//! `#[test]` macro recursion overflows syn's parser. The integration-
//! test path doesn't pull the same expansion.

use std::path::{Path, PathBuf};

use feraille_gpui::shell::path_segments;

#[test]
fn segments_root_only() {
    let segs = path_segments(Path::new("/"));
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].0, "/");
    assert_eq!(segs[0].1, PathBuf::from("/"));
}

#[test]
fn segments_user_home() {
    let segs = path_segments(Path::new("/Users/jkn"));
    let labels: Vec<&str> = segs.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, vec!["/", "Users", "jkn"]);
    assert_eq!(segs.last().unwrap().1, PathBuf::from("/Users/jkn"));
}

#[test]
fn segments_deep_path() {
    let segs = path_segments(Path::new("/Users/jkn/Source/Feraille/crates"));
    assert_eq!(segs.len(), 6);
    assert_eq!(segs[0].0, "/");
    assert_eq!(segs[5].0, "crates");
    assert_eq!(
        segs[5].1,
        PathBuf::from("/Users/jkn/Source/Feraille/crates")
    );
}
