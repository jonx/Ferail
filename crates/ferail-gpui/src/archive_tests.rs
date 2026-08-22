use std::path::PathBuf;

use crate::archive::{
    archive_addition_root, archive_path_at_or_below, project_archive_path,
    unproject_archive_path,
};

#[test]
fn subtree_rename_projects_and_unprojects_every_member() {
    let renames = vec![ferail_fs_native::ArchiveRename {
        from: "old".to_string(),
        to: "new".to_string(),
    }];
    assert_eq!(project_archive_path("old/a.txt", &renames), "new/a.txt");
    assert_eq!(project_archive_path("old/sub/", &renames), "new/sub/");
    assert_eq!(unproject_archive_path("new/a.txt", &renames), "old/a.txt");
    assert_eq!(project_archive_path("older/a.txt", &renames), "older/a.txt");
}

#[test]
fn removal_matching_obeys_path_component_boundaries() {
    assert!(archive_path_at_or_below("folder/a.txt", "folder"));
    assert!(archive_path_at_or_below("folder", "folder"));
    assert!(!archive_path_at_or_below("folder-two/a.txt", "folder"));
}

#[test]
fn addition_root_includes_the_drop_destination() {
    let addition = ferail_fs_native::ArchiveAddition {
        source: PathBuf::from("/tmp/report.txt"),
        destination: "docs/2026".to_string(),
    };
    assert_eq!(archive_addition_root(&addition), "docs/2026/report.txt");
}
