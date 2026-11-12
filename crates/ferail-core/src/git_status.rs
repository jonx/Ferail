//! Read-only Git awareness, the pure half (docs/features/GIT_STATUS.md).
//!
//! [`parse_porcelain_v2`] reads the output of
//! `git status --porcelain=v2 -z --branch` and [`GitTree`] folds the changed
//! paths into per-directory row marks, so a file list can paint one row with
//! one lookup. No I/O happens here: running `git` belongs to
//! `ferail_fs_native::git`, scheduling to the UI crate.

use std::collections::HashMap;

/// The state of one path, ordered by how loudly it should speak when a
/// folder summarizes its contents: a conflict anywhere below outranks a
/// modification, which outranks a staged change, and so on.
///
/// [`GitMark::Ignored`] never propagates to a parent folder: a folder that
/// holds a build directory is not dirty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum GitMark {
    #[default]
    Clean = 0,
    Ignored = 1,
    Untracked = 2,
    Staged = 3,
    Renamed = 4,
    Deleted = 5,
    Modified = 6,
    Conflicted = 7,
}

impl GitMark {
    /// The one-character code drawn in the Git column. These are Git's own
    /// vocabulary, untranslated in every language, like `CPU` / `MEM` in the
    /// status bar.
    pub fn letter(self) -> &'static str {
        match self {
            GitMark::Clean => "",
            GitMark::Ignored => "I",
            GitMark::Untracked => "?",
            GitMark::Staged => "S",
            GitMark::Renamed => "R",
            GitMark::Deleted => "D",
            GitMark::Modified => "M",
            GitMark::Conflicted => "C",
        }
    }

    /// Whether a folder reports this state when something inside it has it.
    pub fn propagates(self) -> bool {
        !matches!(self, GitMark::Clean | GitMark::Ignored)
    }
}

/// What one row of a listing shows. `contents` is true when the mark is a
/// folder's summary of what lies inside it rather than the folder's own
/// state (an untracked or ignored folder carries its own mark).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GitCell {
    pub mark: GitMark,
    pub contents: bool,
}

impl GitCell {
    pub fn is_clean(self) -> bool {
        self.mark == GitMark::Clean
    }
}

/// The `# branch.*` headers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitBranch {
    /// `None` when HEAD is detached.
    pub head: Option<String>,
    /// Full commit id, `None` on an unborn branch (`(initial)`).
    pub oid: Option<String>,
    pub upstream: Option<String>,
    /// `(ahead, behind)`, present only when an upstream is configured and
    /// reachable.
    pub ahead_behind: Option<(u32, u32)>,
}

impl GitBranch {
    /// Abbreviated commit id for a detached HEAD.
    pub fn short_oid(&self) -> Option<&str> {
        self.oid.as_deref().map(|oid| &oid[..oid.len().min(7)])
    }

    pub fn is_detached(&self) -> bool {
        self.head.is_none()
    }
}

/// One changed, untracked or ignored path. `path` is relative to the
/// directory `git` ran in, `/`-separated, without a trailing slash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitEntry {
    pub path: String,
    pub mark: GitMark,
    /// Git reported a directory as a whole (`? dir/`, `! dir/`, or a
    /// submodule).
    pub is_dir: bool,
    pub submodule: bool,
    /// The source of a rename or copy.
    pub orig_path: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitStatus {
    pub branch: GitBranch,
    pub entries: Vec<GitEntry>,
}

/// Parse `git status --porcelain=v2 -z --branch [--ignored]` output.
///
/// Records are NUL-terminated (`-z`), so paths arrive verbatim: spaces,
/// newlines, quotes and non-ASCII names need no unquoting. A rename record
/// (`2`) is followed by one more NUL-terminated field, the original path.
/// Unknown record types are skipped, so a newer `git` that adds one cannot
/// break the column.
pub fn parse_porcelain_v2(bytes: &[u8]) -> GitStatus {
    let mut status = GitStatus::default();
    let mut fields = bytes.split(|b| *b == 0);
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(record);
        let text = text.as_ref();
        match record[0] {
            b'#' => parse_header(text, &mut status.branch),
            b'1' => {
                // 1 XY sub mH mI mW hH hI path
                let parts: Vec<&str> = text.splitn(9, ' ').collect();
                if parts.len() == 9 {
                    push_changed(&mut status, parts[1], parts[2], parts[8], None);
                }
            }
            b'2' => {
                // 2 XY sub mH mI mW hH hI Xscore path NUL origPath
                let parts: Vec<&str> = text.splitn(10, ' ').collect();
                let orig = fields
                    .next()
                    .map(|o| String::from_utf8_lossy(o).into_owned());
                if parts.len() == 10 {
                    push_changed(&mut status, parts[1], parts[2], parts[9], orig);
                }
            }
            b'u' => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                let parts: Vec<&str> = text.splitn(11, ' ').collect();
                if parts.len() == 11 {
                    let (path, is_dir) = split_dir(parts[10]);
                    status.entries.push(GitEntry {
                        path,
                        mark: GitMark::Conflicted,
                        is_dir,
                        submodule: parts[2].starts_with('S'),
                        orig_path: None,
                    });
                }
            }
            b'?' | b'!' => {
                if let Some(path) = text.get(2..) {
                    let (path, is_dir) = split_dir(path);
                    if !path.is_empty() {
                        status.entries.push(GitEntry {
                            path,
                            mark: if record[0] == b'?' {
                                GitMark::Untracked
                            } else {
                                GitMark::Ignored
                            },
                            is_dir,
                            submodule: false,
                            orig_path: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    status
}

fn parse_header(text: &str, branch: &mut GitBranch) {
    let Some(rest) = text.strip_prefix("# ") else {
        return;
    };
    let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
    match key {
        "branch.oid" => {
            branch.oid = (value != "(initial)" && !value.is_empty()).then(|| value.to_string());
        }
        "branch.head" => {
            branch.head = (value != "(detached)" && !value.is_empty()).then(|| value.to_string());
        }
        "branch.upstream" => branch.upstream = Some(value.to_string()),
        "branch.ab" => {
            let mut ahead = None;
            let mut behind = None;
            for part in value.split(' ') {
                if let Some(n) = part.strip_prefix('+') {
                    ahead = n.parse().ok();
                } else if let Some(n) = part.strip_prefix('-') {
                    behind = n.parse().ok();
                }
            }
            if let (Some(a), Some(b)) = (ahead, behind) {
                branch.ahead_behind = Some((a, b));
            }
        }
        _ => {}
    }
}

fn split_dir(path: &str) -> (String, bool) {
    match path.strip_suffix('/') {
        Some(stripped) => (stripped.to_string(), true),
        None => (path.to_string(), false),
    }
}

fn push_changed(
    status: &mut GitStatus,
    xy: &str,
    sub: &str,
    path: &str,
    orig_path: Option<String>,
) {
    let xy = xy.as_bytes();
    if xy.len() != 2 {
        return;
    }
    let renamed = orig_path.is_some();
    let mark = mark_for_xy(xy[0], xy[1], renamed);
    if mark == GitMark::Clean {
        return;
    }
    let submodule = sub.starts_with('S');
    let (path, is_dir) = split_dir(path);
    status.entries.push(GitEntry {
        path,
        mark,
        is_dir: is_dir || submodule,
        submodule,
        orig_path,
    });
}

/// `X` is the index (staged) side, `Y` the work tree. Unstaged work wins
/// the cell: it is what the user has not dealt with yet.
fn mark_for_xy(x: u8, y: u8, renamed: bool) -> GitMark {
    match y {
        b'M' | b'T' => return GitMark::Modified,
        b'D' => return GitMark::Deleted,
        _ => {}
    }
    if renamed || x == b'R' || x == b'C' {
        GitMark::Renamed
    } else if x == b'D' {
        GitMark::Deleted
    } else if x != b'.' {
        GitMark::Staged
    } else {
        GitMark::Clean
    }
}

/// Changed paths folded into per-directory row marks.
///
/// `dirs` maps a repository-relative directory (`""` is the root) to the
/// marks of its direct children, keyed by on-disk leaf name. A child folder
/// holds its own mark when Git reported it whole, otherwise the loudest
/// propagating mark of everything below it.
#[derive(Clone, Debug, Default)]
pub struct GitTree {
    dirs: HashMap<String, HashMap<String, GitCell>>,
    /// Folders Git reported whole (untracked or ignored): everything inside
    /// them shares the mark, and Git lists none of it.
    whole_dirs: HashMap<String, GitMark>,
    /// Dirty submodules. A folder inside one belongs to another repository.
    submodules: Vec<String>,
}

impl GitTree {
    pub fn build(entries: &[GitEntry]) -> Self {
        let mut tree = GitTree::default();
        for entry in entries {
            if entry.path.is_empty() {
                continue;
            }
            if entry.is_dir && matches!(entry.mark, GitMark::Untracked | GitMark::Ignored) {
                tree.whole_dirs.insert(entry.path.clone(), entry.mark);
            }
            if entry.submodule {
                tree.submodules.push(entry.path.clone());
            }
            let (parent, leaf) = split_parent(&entry.path);
            tree.dirs
                .entry(parent.to_string())
                .or_default()
                .insert(
                    leaf.to_string(),
                    GitCell {
                        mark: entry.mark,
                        contents: false,
                    },
                );
            if !entry.mark.propagates() {
                continue;
            }
            // Every ancestor folder summarizes the change, unless it carries
            // its own mark (a submodule, an untracked folder).
            let mut dir = parent;
            while !dir.is_empty() {
                let (up, name) = split_parent(dir);
                let slot = tree
                    .dirs
                    .entry(up.to_string())
                    .or_default()
                    .entry(name.to_string())
                    .or_insert(GitCell {
                        mark: GitMark::Clean,
                        contents: true,
                    });
                if slot.contents && entry.mark > slot.mark {
                    slot.mark = entry.mark;
                }
                dir = up;
            }
        }
        tree
    }

    /// The marks for the rows of one directory, `rel_dir` relative to the
    /// repository root (`""` for the root itself).
    pub fn dir_view(&self, rel_dir: &str) -> DirView<'_> {
        let inherited = ancestors_inclusive(rel_dir).find_map(|dir| self.whole_dirs.get(dir).copied());
        DirView {
            inherited,
            children: self.dirs.get(rel_dir),
        }
    }

    /// Whether a folder may belong to a different repository than the one
    /// this tree describes: it sits inside a submodule, or inside a folder
    /// the outer repository does not track, where a nested repository can
    /// start. The caller asks `git` instead of assuming.
    pub fn may_hold_nested_repo(&self, rel_dir: &str) -> bool {
        if rel_dir.is_empty() {
            return false;
        }
        ancestors_inclusive(rel_dir)
            .any(|dir| self.whole_dirs.contains_key(dir) || self.submodules.iter().any(|s| s == dir))
    }
}

/// See [`GitTree::dir_view`].
#[derive(Clone, Copy, Debug)]
pub struct DirView<'a> {
    inherited: Option<GitMark>,
    children: Option<&'a HashMap<String, GitCell>>,
}

impl DirView<'_> {
    /// The cell for the child named `leaf` (the on-disk name).
    pub fn cell(&self, leaf: &str) -> GitCell {
        if let Some(mark) = self.inherited {
            return GitCell {
                mark,
                contents: false,
            };
        }
        self.children
            .and_then(|children| children.get(leaf).copied())
            .unwrap_or_default()
    }

    /// Whether no row of this directory can carry a mark.
    pub fn is_clean(&self) -> bool {
        self.inherited.is_none() && self.children.is_none_or(|c| c.is_empty())
    }
}

fn split_parent(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(ix) => (&path[..ix], &path[ix + 1..]),
        None => ("", path),
    }
}

/// `a/b/c`, `a/b`, `a`.
fn ancestors_inclusive(path: &str) -> impl Iterator<Item = &str> {
    let mut next = (!path.is_empty()).then_some(path);
    std::iter::from_fn(move || {
        let current = next?;
        next = current.rfind('/').map(|ix| &current[..ix]);
        Some(current)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(records: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in records {
            out.extend_from_slice(r.as_bytes());
            out.push(0);
        }
        out
    }

    const SHA: &str = "1111111111111111111111111111111111111111";

    fn ordinary(xy: &str, path: &str) -> String {
        format!("1 {xy} N... 100644 100644 100644 {SHA} {SHA} {path}")
    }

    fn mark_of(status: &GitStatus, path: &str) -> GitMark {
        status
            .entries
            .iter()
            .find(|e| e.path == path)
            .map(|e| e.mark)
            .unwrap_or(GitMark::Clean)
    }

    #[test]
    fn branch_headers_with_upstream() {
        let s = parse_porcelain_v2(&z(&[
            &format!("# branch.oid {SHA}"),
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -5",
        ]));
        assert_eq!(s.branch.head.as_deref(), Some("main"));
        assert_eq!(s.branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!(s.branch.ahead_behind, Some((2, 5)));
        assert_eq!(s.branch.short_oid(), Some("1111111"));
        assert!(!s.branch.is_detached());
        assert!(s.entries.is_empty());
    }

    #[test]
    fn branch_headers_without_upstream() {
        let s = parse_porcelain_v2(&z(&[&format!("# branch.oid {SHA}"), "# branch.head feature/x"]));
        assert_eq!(s.branch.head.as_deref(), Some("feature/x"));
        assert_eq!(s.branch.upstream, None);
        assert_eq!(s.branch.ahead_behind, None);
    }

    #[test]
    fn detached_head_and_unborn_branch() {
        let s = parse_porcelain_v2(&z(&[
            "# branch.oid abcdef0123456789abcdef0123456789abcdef01",
            "# branch.head (detached)",
        ]));
        assert!(s.branch.is_detached());
        assert_eq!(s.branch.short_oid(), Some("abcdef0"));

        let s = parse_porcelain_v2(&z(&["# branch.oid (initial)", "# branch.head main"]));
        assert_eq!(s.branch.oid, None);
        assert_eq!(s.branch.head.as_deref(), Some("main"));
    }

    #[test]
    fn ordinary_changed_entries() {
        let s = parse_porcelain_v2(&z(&[
            &ordinary(".M", "worktree.txt"),
            &ordinary("M.", "staged.txt"),
            &ordinary("MM", "both.txt"),
            &ordinary("A.", "added.txt"),
            &ordinary("D.", "staged-delete.txt"),
            &ordinary(".D", "worktree-delete.txt"),
            &ordinary(".T", "typechange"),
        ]));
        assert_eq!(mark_of(&s, "worktree.txt"), GitMark::Modified);
        assert_eq!(mark_of(&s, "staged.txt"), GitMark::Staged);
        assert_eq!(mark_of(&s, "both.txt"), GitMark::Modified);
        assert_eq!(mark_of(&s, "added.txt"), GitMark::Staged);
        assert_eq!(mark_of(&s, "staged-delete.txt"), GitMark::Deleted);
        assert_eq!(mark_of(&s, "worktree-delete.txt"), GitMark::Deleted);
        assert_eq!(mark_of(&s, "typechange"), GitMark::Modified);
    }

    #[test]
    fn rename_consumes_the_original_path_field() {
        let s = parse_porcelain_v2(&z(&[
            &format!("2 R. N... 100644 100644 100644 {SHA} {SHA} R100 new name.txt"),
            "old name.txt",
            "? after.txt",
        ]));
        assert_eq!(s.entries.len(), 2, "origPath is not a record of its own");
        let renamed = &s.entries[0];
        assert_eq!(renamed.path, "new name.txt");
        assert_eq!(renamed.mark, GitMark::Renamed);
        assert_eq!(renamed.orig_path.as_deref(), Some("old name.txt"));
        assert_eq!(mark_of(&s, "after.txt"), GitMark::Untracked);
    }

    #[test]
    fn renamed_then_edited_reads_modified() {
        let s = parse_porcelain_v2(&z(&[
            &format!("2 RM N... 100644 100644 100644 {SHA} {SHA} R90 b.txt"),
            "a.txt",
        ]));
        assert_eq!(mark_of(&s, "b.txt"), GitMark::Modified);
    }

    #[test]
    fn unmerged_entries_are_conflicted() {
        let s = parse_porcelain_v2(&z(&[&format!(
            "u UU N... 100644 100644 100644 100644 {SHA} {SHA} {SHA} src/conflict.rs"
        )]));
        assert_eq!(mark_of(&s, "src/conflict.rs"), GitMark::Conflicted);
    }

    #[test]
    fn untracked_and_ignored_files_and_folders() {
        let s = parse_porcelain_v2(&z(&["? notes.md", "? new-dir/", "! target/", "! debug.log"]));
        assert_eq!(mark_of(&s, "notes.md"), GitMark::Untracked);
        let dir = s.entries.iter().find(|e| e.path == "new-dir").unwrap();
        assert!(dir.is_dir);
        assert_eq!(dir.mark, GitMark::Untracked);
        let target = s.entries.iter().find(|e| e.path == "target").unwrap();
        assert!(target.is_dir);
        assert_eq!(target.mark, GitMark::Ignored);
        assert_eq!(mark_of(&s, "debug.log"), GitMark::Ignored);
    }

    #[test]
    fn awkward_paths_survive_verbatim() {
        let s = parse_porcelain_v2(&z(&[
            &ordinary(".M", "dir with spaces/file name.txt"),
            "? caf\u{e9} \u{1F600}.txt",
            "? line\nbreak.txt",
            "? \"quoted\".txt",
        ]));
        assert_eq!(mark_of(&s, "dir with spaces/file name.txt"), GitMark::Modified);
        assert_eq!(mark_of(&s, "caf\u{e9} \u{1F600}.txt"), GitMark::Untracked);
        assert_eq!(mark_of(&s, "line\nbreak.txt"), GitMark::Untracked);
        assert_eq!(mark_of(&s, "\"quoted\".txt"), GitMark::Untracked);
    }

    #[test]
    fn submodule_entries_are_directories() {
        let s = parse_porcelain_v2(&z(&[&format!(
            "1 .M SCM. 160000 160000 160000 {SHA} {SHA} vendor/lib"
        )]));
        let sub = &s.entries[0];
        assert_eq!(sub.path, "vendor/lib");
        assert!(sub.submodule);
        assert!(sub.is_dir);
        assert_eq!(sub.mark, GitMark::Modified);
    }

    #[test]
    fn junk_and_unknown_records_are_skipped() {
        let s = parse_porcelain_v2(b"\0\0# stash 3\0x what\x001 M\0");
        assert!(s.entries.is_empty());
        assert_eq!(s.branch, GitBranch::default());
        assert_eq!(parse_porcelain_v2(b""), GitStatus::default());
    }

    #[test]
    fn folders_summarize_their_contents() {
        let s = parse_porcelain_v2(&z(&[
            &ordinary(".M", "src/ui/view.rs"),
            &ordinary("A.", "src/new.rs"),
            "? src/scratch.txt",
            "! src/build/",
            "! top.log",
            &ordinary("M.", "docs/readme.md"),
        ]));
        let tree = GitTree::build(&s.entries);
        let root = tree.dir_view("");
        assert_eq!(
            root.cell("src"),
            GitCell {
                mark: GitMark::Modified,
                contents: true
            }
        );
        assert_eq!(
            root.cell("docs"),
            GitCell {
                mark: GitMark::Staged,
                contents: true
            }
        );
        assert_eq!(root.cell("top.log").mark, GitMark::Ignored);
        assert!(root.cell("Cargo.toml").is_clean());

        let src = tree.dir_view("src");
        assert_eq!(src.cell("ui").mark, GitMark::Modified);
        assert!(src.cell("ui").contents);
        assert_eq!(src.cell("new.rs").mark, GitMark::Staged);
        assert!(!src.cell("new.rs").contents);
        assert_eq!(src.cell("scratch.txt").mark, GitMark::Untracked);
        // Ignored folders keep their own mark and never dirty the parent.
        assert_eq!(src.cell("build").mark, GitMark::Ignored);

        let ui = tree.dir_view("src/ui");
        assert_eq!(ui.cell("view.rs").mark, GitMark::Modified);
    }

    #[test]
    fn conflict_wins_over_modified_and_deleted_counts() {
        let s = parse_porcelain_v2(&z(&[
            &ordinary(".M", "a/one.txt"),
            &format!("u AA N... 100644 100644 100644 100644 {SHA} {SHA} {SHA} a/deep/two.txt"),
            &ordinary(".M", "b/x.txt"),
            &ordinary(".D", "c/gone.txt"),
            "? d/new.txt",
        ]));
        let root = GitTree::build(&s.entries);
        let root = root.dir_view("");
        assert_eq!(root.cell("a").mark, GitMark::Conflicted);
        assert_eq!(root.cell("b").mark, GitMark::Modified);
        assert_eq!(root.cell("c").mark, GitMark::Deleted);
        assert_eq!(root.cell("d").mark, GitMark::Untracked);
    }

    #[test]
    fn inside_an_untracked_folder_everything_is_untracked() {
        let s = parse_porcelain_v2(&z(&["? fresh/", "! out/"]));
        let tree = GitTree::build(&s.entries);
        assert_eq!(tree.dir_view("fresh").cell("anything").mark, GitMark::Untracked);
        assert_eq!(tree.dir_view("fresh/deeper").cell("x").mark, GitMark::Untracked);
        assert_eq!(tree.dir_view("out/debug").cell("bin").mark, GitMark::Ignored);
        assert!(tree.may_hold_nested_repo("fresh/deeper"));
        assert!(!tree.may_hold_nested_repo(""));
        assert!(!tree.may_hold_nested_repo("src"));
        assert!(tree.dir_view("src").is_clean());
    }

    #[test]
    fn submodule_keeps_its_own_mark_and_may_nest() {
        let s = parse_porcelain_v2(&z(&[
            &format!("1 .M SC.. 160000 160000 160000 {SHA} {SHA} vendor/lib"),
            &ordinary(".M", "vendor/readme.md"),
        ]));
        let tree = GitTree::build(&s.entries);
        let vendor = tree.dir_view("vendor");
        assert_eq!(
            vendor.cell("lib"),
            GitCell {
                mark: GitMark::Modified,
                contents: false
            }
        );
        assert!(tree.may_hold_nested_repo("vendor/lib"));
        assert!(tree.may_hold_nested_repo("vendor/lib/src"));
        assert!(!tree.may_hold_nested_repo("vendor"));
    }

    #[test]
    fn ancestors_walk_up_to_the_top() {
        let all: Vec<&str> = ancestors_inclusive("a/b/c").collect();
        assert_eq!(all, ["a/b/c", "a/b", "a"]);
        assert_eq!(ancestors_inclusive("").count(), 0);
    }
}
