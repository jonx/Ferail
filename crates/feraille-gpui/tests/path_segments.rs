//! Pure-domain test for the breadcrumb path-splitter.
//!
//! Lives as an integration test (not `#[cfg(test)] mod`) because the
//! inline form crashes the compiler — gpui's type graph plus the
//! `#[test]` macro recursion overflows syn's parser. The integration-
//! test path doesn't pull the same expansion.

use std::path::{Path, PathBuf};

use feraille_gpui::shell::path_segments;

#[cfg(unix)]
#[test]
fn segments_root_only() {
    let segs = path_segments(Path::new("/"));
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].0, "/");
    assert_eq!(segs[0].1, PathBuf::from("/"));
}

#[cfg(unix)]
#[test]
fn segments_user_home() {
    let segs = path_segments(Path::new("/Users/jkn"));
    let labels: Vec<&str> = segs.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, vec!["/", "Users", "jkn"]);
    assert_eq!(segs.last().unwrap().1, PathBuf::from("/Users/jkn"));
}

#[cfg(unix)]
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

#[cfg(windows)]
#[test]
fn segments_drive_root_only() {
    let segs = path_segments(Path::new(r"C:\"));
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].0, r"C:\");
    assert_eq!(segs[0].1, PathBuf::from(r"C:\"));
}

#[cfg(windows)]
#[test]
fn segments_drive_user_home() {
    let segs = path_segments(Path::new(r"C:\Users\JohnKNIPPER"));
    let labels: Vec<&str> = segs.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, vec![r"C:\", "Users", "JohnKNIPPER"]);
    assert_eq!(
        segs.last().unwrap().1,
        PathBuf::from(r"C:\Users\JohnKNIPPER")
    );
}

#[cfg(windows)]
#[test]
fn segments_drive_deep_path() {
    let segs = path_segments(Path::new(r"D:\Source\Feraille\crates"));
    assert_eq!(segs.len(), 4);
    assert_eq!(segs[0].0, r"D:\");
    assert_eq!(segs[3].0, "crates");
    assert_eq!(
        segs[3].1,
        PathBuf::from(r"D:\Source\Feraille\crates")
    );
}

// ---- canonicalize_for_identity (path-identity contract boundary) ----

#[cfg(unix)]
#[test]
fn canonicalize_resolves_symlinks_for_identity() {
    use feraille_gpui::shell::canonicalize_for_identity;
    let base = std::env::temp_dir().join(format!(
        "feraille-canon-test-{}",
        std::process::id()
    ));
    let real = base.join("real");
    let link = base.join("link");
    std::fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let via_link = canonicalize_for_identity(link.clone());
    let via_real = canonicalize_for_identity(real.clone());
    // The two spellings converge on one identity key.
    assert_eq!(via_link, via_real);

    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn canonicalize_falls_back_on_missing_path() {
    use feraille_gpui::shell::canonicalize_for_identity;
    let ghost = PathBuf::from("/definitely/not/a/real/path/feraille");
    assert_eq!(canonicalize_for_identity(ghost.clone()), ghost);
}
