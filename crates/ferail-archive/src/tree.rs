//! Turning a flat [`Toc`] into an expandable tree.
//!
//! An archive's table of contents is a flat list of `/`-separated paths, and
//! most writers record only *files*: the directories are implied by those
//! paths (a zip of `ares.app/Contents/MacOS/ares` may contain no directory
//! record at all). Showing that list flat is unusable at real archive sizes:
//! the 4990-entry app bundle that motivated this renders every nested resource
//! at the top level.
//!
//! [`ArchiveTree`] indexes the flat list once (synthesizing the missing
//! directories), then [`ArchiveTree::visible_rows`] projects the subset the
//! user has actually expanded. Pure logic: the view owns the `expanded` set
//! and does nothing but render what comes back.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::entry::{ArchiveEntry, Toc};

/// One row to draw: a node in the tree at a known depth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// Full `/`-separated path inside the archive. Directory paths carry no
    /// trailing slash, so they can be compared with (and passed to) the
    /// extraction selection API directly.
    pub path: String,
    /// Leaf name to display.
    pub name: String,
    /// Nesting level; 0 for top-level entries.
    pub depth: usize,
    pub is_dir: bool,
    /// Whether this directory has any children (only `true` for directories).
    /// Childless directories draw no caret.
    pub expandable: bool,
    /// Whether this directory is currently open.
    pub expanded: bool,
    /// Uncompressed size: `None` for directories and for formats that don't
    /// record it.
    pub size: Option<u64>,
    pub compressed_size: Option<u64>,
    pub mtime_unix: Option<i64>,
    pub compression_method: Option<String>,
    pub checksum: Option<String>,
    pub unix_mode: Option<u32>,
    pub comment: Option<String>,
    pub encrypted: bool,
}

/// A directory-indexed view over a [`Toc`].
#[derive(Debug, Default)]
pub struct ArchiveTree {
    /// Parent directory path → its children's paths, sorted. The root is `""`.
    children: BTreeMap<String, BTreeSet<String>>,
    /// Every path known to be a directory (recorded or implied).
    dirs: HashSet<String>,
    /// File metadata by path, for the rows that have it.
    files: BTreeMap<String, FileFacts>,
}

#[derive(Debug, Clone, Default)]
struct FileFacts {
    size: Option<u64>,
    compressed_size: Option<u64>,
    mtime_unix: Option<i64>,
    compression_method: Option<String>,
    checksum: Option<String>,
    unix_mode: Option<u32>,
    comment: Option<String>,
    encrypted: bool,
}

impl ArchiveTree {
    /// Index `toc`, synthesizing every directory implied by an entry path.
    pub fn build(toc: &Toc) -> ArchiveTree {
        let mut tree = ArchiveTree::default();
        for entry in &toc.entries {
            tree.insert(entry);
        }
        tree
    }

    fn insert(&mut self, entry: &ArchiveEntry) {
        let path = normalize(&entry.path);
        if path.is_empty() {
            return;
        }
        // Link every ancestor, creating the implied directories on the way.
        let mut parent = String::new();
        let parts: Vec<&str> = path.split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            let is_last = i + 1 == parts.len();
            let full = if parent.is_empty() {
                (*part).to_string()
            } else {
                format!("{parent}/{part}")
            };
            self.children
                .entry(parent.clone())
                .or_default()
                .insert(full.clone());
            // Everything but the final component is necessarily a directory;
            // the final one is a directory only if the entry says so.
            if !is_last || entry.is_dir {
                self.dirs.insert(full.clone());
            } else {
                self.files.insert(
                    full.clone(),
                    FileFacts {
                        size: entry.uncompressed_size,
                        compressed_size: entry.compressed_size,
                        mtime_unix: entry.mtime_unix,
                        compression_method: entry.compression_method.clone(),
                        checksum: entry.checksum.clone(),
                        unix_mode: entry.unix_mode,
                        comment: entry.comment.clone(),
                        encrypted: entry.encrypted,
                    },
                );
            }
            parent = full;
        }
    }

    /// Whether `path` is a directory in this archive.
    pub fn is_dir(&self, path: &str) -> bool {
        self.dirs.contains(normalize(path).as_str())
    }

    /// Rows to draw, in display order, given the currently open directories.
    /// Directories sort before files, then case-insensitively by name,
    /// matching the file list's folders-first default.
    pub fn visible_rows(&self, expanded: &HashSet<String>) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        self.push_level("", 0, expanded, &mut rows);
        rows
    }

    fn push_level(
        &self,
        parent: &str,
        depth: usize,
        expanded: &HashSet<String>,
        out: &mut Vec<TreeRow>,
    ) {
        let Some(kids) = self.children.get(parent) else {
            return;
        };
        let mut ordered: Vec<&String> = kids.iter().collect();
        ordered.sort_by(|a, b| {
            let a_dir = self.dirs.contains(*a);
            let b_dir = self.dirs.contains(*b);
            b_dir
                .cmp(&a_dir)
                .then_with(|| leaf(a).to_lowercase().cmp(&leaf(b).to_lowercase()))
        });
        for path in ordered {
            let is_dir = self.dirs.contains(path);
            let is_expanded = is_dir && expanded.contains(path);
            let facts = self.files.get(path).cloned().unwrap_or_default();
            out.push(TreeRow {
                path: path.clone(),
                name: leaf(path).to_string(),
                depth,
                is_dir,
                expandable: is_dir && self.children.contains_key(path),
                expanded: is_expanded,
                size: if is_dir { None } else { facts.size },
                compressed_size: if is_dir { None } else { facts.compressed_size },
                mtime_unix: facts.mtime_unix,
                compression_method: facts.compression_method,
                checksum: facts.checksum,
                unix_mode: facts.unix_mode,
                comment: facts.comment,
                encrypted: facts.encrypted,
            });
            if is_expanded {
                self.push_level(path, depth + 1, expanded, out);
            }
        }
    }

    /// Every path in the archive whose leaf name contains `needle`
    /// (case-insensitive), as a flat result list: the tree's answer to the
    /// filter box. Directories are included so a folder can still be selected
    /// (and extracted whole) from a filtered view.
    pub fn matching_rows(&self, needle: &str) -> Vec<TreeRow> {
        let needle = needle.to_lowercase();
        let mut rows: Vec<TreeRow> = self
            .all_paths()
            .filter(|p| leaf(p).to_lowercase().contains(&needle))
            .map(|path| {
                let is_dir = self.dirs.contains(&path);
                let facts = self.files.get(&path).cloned().unwrap_or_default();
                TreeRow {
                    name: leaf(&path).to_string(),
                    depth: 0,
                    is_dir,
                    expandable: false,
                    expanded: false,
                    size: if is_dir { None } else { facts.size },
                    compressed_size: if is_dir { None } else { facts.compressed_size },
                    mtime_unix: facts.mtime_unix,
                    compression_method: facts.compression_method,
                    checksum: facts.checksum,
                    unix_mode: facts.unix_mode,
                    comment: facts.comment,
                    encrypted: facts.encrypted,
                    path,
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        rows
    }

    fn all_paths(&self) -> impl Iterator<Item = String> + '_ {
        self.children.values().flatten().cloned()
    }

    /// Total number of nodes (files + directories, including synthesized ones).
    pub fn len(&self) -> usize {
        self.children.values().map(|c| c.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Strip trailing/leading slashes so `"a/b/"` and `"a/b"` are one node.
fn normalize(path: &str) -> String {
    path.trim_matches('/').replace("//", "/")
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> ArchiveEntry {
        ArchiveEntry {
            path: path.to_string(),
            is_dir: false,
            uncompressed_size: Some(size),
            compressed_size: None,
            mtime_unix: None,
            compression_method: None,
            checksum: None,
            unix_mode: None,
            comment: None,
            encrypted: false,
        }
    }

    /// A zip that records only files: the common case, and the one that made
    /// the flat list unusable.
    fn implied_toc() -> Toc {
        Toc {
            entries: vec![
                file("ares-v148/ares.app/Contents/MacOS/ares", 43_800_000),
                file("ares-v148/ares.app/Contents/CodeResources", 1_800),
                file("ares-v148/README.md", 120),
            ],
            needs_password: false,
        }
    }

    #[test]
    fn synthesizes_directories_that_the_archive_never_recorded() {
        let tree = ArchiveTree::build(&implied_toc());
        assert!(tree.is_dir("ares-v148"));
        assert!(tree.is_dir("ares-v148/ares.app"));
        assert!(tree.is_dir("ares-v148/ares.app/Contents"));
        assert!(!tree.is_dir("ares-v148/README.md"));
    }

    #[test]
    fn collapsed_by_default_shows_only_the_top_level() {
        let tree = ArchiveTree::build(&implied_toc());
        let rows = tree.visible_rows(&HashSet::new());
        assert_eq!(rows.len(), 1, "got {rows:#?}");
        assert_eq!(rows[0].name, "ares-v148");
        assert!(rows[0].is_dir);
        assert!(rows[0].expandable);
        assert!(!rows[0].expanded);
    }

    #[test]
    fn expanding_reveals_exactly_one_more_level() {
        let tree = ArchiveTree::build(&implied_toc());
        let expanded: HashSet<String> = ["ares-v148".to_string()].into_iter().collect();
        let rows = tree.visible_rows(&expanded);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        // Folders first, then files; the app bundle's *contents* stay hidden.
        assert_eq!(names, vec!["ares-v148", "ares.app", "README.md"]);
        assert_eq!(rows[1].depth, 1);
        assert!(rows[1].expandable);
    }

    #[test]
    fn sizes_land_on_files_not_directories() {
        let tree = ArchiveTree::build(&implied_toc());
        let expanded: HashSet<String> = ["ares-v148".to_string()].into_iter().collect();
        let rows = tree.visible_rows(&expanded);
        let readme = rows.iter().find(|r| r.name == "README.md").unwrap();
        assert_eq!(readme.size, Some(120));
        let app = rows.iter().find(|r| r.name == "ares.app").unwrap();
        assert_eq!(app.size, None);
    }

    #[test]
    fn explicit_directory_entries_do_not_duplicate_implied_ones() {
        let toc = Toc {
            entries: vec![
                ArchiveEntry {
                    path: "project/".to_string(),
                    is_dir: true,
                    uncompressed_size: None,
                    compressed_size: None,
                    mtime_unix: None,
                    compression_method: None,
                    checksum: None,
                    unix_mode: None,
                    comment: None,
                    encrypted: false,
                },
                file("project/a.txt", 5),
            ],
            needs_password: false,
        };
        let tree = ArchiveTree::build(&toc);
        let rows = tree.visible_rows(&HashSet::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "project");
        assert!(rows[0].is_dir);
    }

    #[test]
    fn filter_matches_leaf_names_anywhere_in_the_tree() {
        let tree = ArchiveTree::build(&implied_toc());
        let hits = tree.matching_rows("ares");
        let names: Vec<&str> = hits.iter().map(|r| r.name.as_str()).collect();
        // "ares-v148" (dir), "ares.app" (dir), then the "ares" binary.
        assert!(names.contains(&"ares"), "got {names:?}");
        assert!(names.contains(&"ares.app"), "got {names:?}");
        // Non-matching leaves are gone.
        assert!(!names.contains(&"README.md"));
    }

    #[test]
    fn empty_and_slash_only_paths_are_ignored() {
        let toc = Toc {
            entries: vec![file("", 0), file("/", 0), file("ok.txt", 1)],
            needs_password: false,
        };
        let tree = ArchiveTree::build(&toc);
        let rows = tree.visible_rows(&HashSet::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "ok.txt");
    }
}
