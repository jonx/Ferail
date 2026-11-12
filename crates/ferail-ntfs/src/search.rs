//! Name search over a [`CompactNtfsIndex`] subtree.
//!
//! The matching rules reproduce the portable recursive walker
//! (`ferail_fs_native::search`) so a Fast NTFS search and a Portable search
//! of the same tree return the same entries:
//!
//! - an entry whose name starts with `.` or that carries
//!   `FILE_ATTRIBUTE_HIDDEN` is neither matched nor descended unless
//!   `include_hidden` is set;
//! - the haystack is the entry name, or with `match_path` its path relative
//!   to the search root joined with `\`, decoded with
//!   `String::from_utf16_lossy` and lowercased with `str::to_lowercase`;
//! - every text term must be a substring of that haystack (the AND of
//!   `FilterExpr::text_matches`); an empty term list matches every entry;
//! - a directory whose extension is a package extension is matched but not
//!   descended unless `descend_packages` is set (`Path::extension` rules);
//! - each hard-link name is listed under its own parent with the file's full
//!   sizes.
//!
//! Reparse directories follow the Disk Usage walk: a directory with real
//! indexed children (a cloud-sync folder) is descended, one without (a
//! junction, mount point or directory symlink) is listed but never entered.
//! The external target of a reparse point is never resolved.

use crate::index::{CompactNtfsIndex, FILE_ATTRIBUTE_DIRECTORY, MAX_BATCH_ROWS};
use crate::{ErrorKind, FileReference, NtfsError, Result};

/// Most text terms one search request may carry.
pub const MAX_SEARCH_TERMS: usize = 64;
/// Longest single text term, in UTF-16 code units.
pub const MAX_SEARCH_TERM_UNITS: usize = 1024;
/// Longest relative path a search row may carry, in UTF-16 code units.
pub const MAX_RELATIVE_PATH_UNITS: usize = 32_767;
/// Path code units one search batch may carry in total, so an encoded batch
/// stays far below the 16 MiB frame cap whatever the path lengths.
pub(crate) const SEARCH_BATCH_UNIT_BUDGET: usize = 1 << 20;
pub(crate) const PATH_SEPARATOR: u16 = b'\\' as u16;
const DOT: u16 = b'.' as u16;
const PACKAGE_EXTENSIONS: [&str; 6] = ["app", "bundle", "framework", "plugin", "kext", "xcodeproj"];
const WINDOWS_TO_UNIX_SECONDS: u64 = 11_644_473_600;
const TICKS_PER_SECOND: u64 = 10_000_000;

/// What to look for. `terms` are matched as given against the lowercased
/// haystack, exactly like `FilterExpr::text_matches`: pass the expression's
/// text terms, or `vec![needle.to_lowercase()]` for a plain needle.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchQuery {
    pub terms: Vec<String>,
    pub match_path: bool,
    pub include_hidden: bool,
    pub descend_packages: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchEntryKind {
    File,
    Directory,
    /// A non-directory with a reparse point: file symlink, cloud placeholder,
    /// deduplicated file.
    ReparseFile,
    /// A directory with a reparse point: junction, mount point, directory
    /// symlink, or a cloud-sync folder.
    ReparseDirectory,
}

/// One search hit. `relative_path` is the raw UTF-16 path below the search
/// root, components joined by `\`, never empty, never starting or ending
/// with a separator. Ill-formed UTF-16 (unpaired surrogates) is preserved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRow {
    pub relative_path: Vec<u16>,
    pub file_record: FileReference,
    pub kind: SearchEntryKind,
    /// The entry carries a cloud-files reparse tag (OneDrive and similar).
    pub cloud: bool,
    /// Win32 `FILE_ATTRIBUTE_*` bits from `$STANDARD_INFORMATION`, with
    /// `FILE_ATTRIBUTE_DIRECTORY` added for directories.
    pub attributes: u32,
    /// Logical size of the unnamed data stream; 0 for directories.
    pub logical_bytes: u64,
    /// Bytes on disk of the unnamed data stream; 0 for directories.
    pub allocated_bytes: u64,
    /// Raw NTFS ticks; see [`ntfs_ticks_to_unix_seconds`].
    pub modified_ticks: u64,
    pub created_ticks: u64,
}

impl SearchRow {
    /// The last path component.
    pub fn name(&self) -> &[u16] {
        match self
            .relative_path
            .iter()
            .rposition(|unit| *unit == PATH_SEPARATOR)
        {
            Some(separator) => &self.relative_path[separator + 1..],
            None => &self.relative_path,
        }
    }

    pub fn display_name(&self) -> String {
        String::from_utf16_lossy(self.name())
    }

    pub fn is_directory(&self) -> bool {
        matches!(
            self.kind,
            SearchEntryKind::Directory | SearchEntryKind::ReparseDirectory
        )
    }

    /// The walker's hidden rule: a leading dot or `FILE_ATTRIBUTE_HIDDEN`.
    pub fn is_hidden(&self) -> bool {
        self.name().first() == Some(&DOT) || self.attributes & crate::FILE_ATTRIBUTE_HIDDEN != 0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SearchSummary {
    pub matches: u64,
    /// Entries considered (after hidden pruning).
    pub entries_visited: u64,
    pub directories_descended: u64,
    /// Entries that cannot be reported: a relative path longer than
    /// [`MAX_RELATIVE_PATH_UNITS`], or a name the wire refuses (`/`, NUL,
    /// `.`, `..`; none is a legal NTFS name). Their subtrees are not visited.
    pub skipped_paths: u64,
    pub cycles_skipped: u64,
}

/// Converts raw NTFS ticks to Unix seconds with the same rule the portable
/// walker applies (`SystemTime::duration_since(UNIX_EPOCH)` then whole
/// seconds): anything before 1970, including the 0 "unset" value, is `None`.
pub fn ntfs_ticks_to_unix_seconds(ticks: u64) -> Option<i64> {
    let seconds = ticks / TICKS_PER_SECOND;
    let unix = seconds.checked_sub(WINDOWS_TO_UNIX_SECONDS)?;
    i64::try_from(unix).ok()
}

/// `Path::extension` on the component, compared case-insensitively against
/// the package list. A name whose only dot is the first character has no
/// extension, and a non-UTF-16 extension never matches.
pub(crate) fn is_package_component(name: &[u16]) -> bool {
    let Some(dot) = name.iter().rposition(|unit| *unit == DOT) else {
        return false;
    };
    if dot == 0 {
        return false;
    }
    let extension = &name[dot + 1..];
    PACKAGE_EXTENSIONS.iter().any(|candidate| {
        extension.len() == candidate.len()
            && extension.iter().zip(candidate.bytes()).all(|(unit, byte)| {
                u8::try_from(*unit).is_ok_and(|unit| unit.eq_ignore_ascii_case(&byte))
            })
    })
}

fn reportable_name(name: &[u16]) -> bool {
    const SLASH: u16 = b'/' as u16;
    name != [DOT]
        && name != [DOT, DOT]
        && !name
            .iter()
            .any(|unit| *unit == 0 || *unit == SLASH || *unit == PATH_SEPARATOR)
}

/// Lowercase-and-contains matcher with an allocation-free ASCII fast path.
/// For an all-ASCII haystack `to_lowercase` is ASCII lowercasing, and a term
/// containing any non-ASCII character can never be its substring, so the
/// fast path is exact. Everything else takes the walker's literal route.
struct TextMatcher {
    terms: Vec<String>,
    ascii: Vec<u8>,
}

impl TextMatcher {
    fn new(terms: &[String]) -> Self {
        Self {
            // "" is a substring of everything; dropping it is equivalent.
            terms: terms.iter().filter(|t| !t.is_empty()).cloned().collect(),
            ascii: Vec::new(),
        }
    }

    fn matches_everything(&self) -> bool {
        self.terms.is_empty()
    }

    fn matches(&mut self, haystack: &[u16]) -> bool {
        if self.terms.is_empty() {
            return true;
        }
        if haystack.iter().all(|unit| *unit < 0x80) {
            self.ascii.clear();
            self.ascii.extend(
                haystack
                    .iter()
                    .map(|unit| (*unit as u8).to_ascii_lowercase()),
            );
            let text = std::str::from_utf8(&self.ascii).expect("ASCII is UTF-8");
            return self
                .terms
                .iter()
                .all(|term| term.is_ascii() && text.contains(term.as_str()));
        }
        let lowered = String::from_utf16_lossy(haystack).to_lowercase();
        self.terms
            .iter()
            .all(|term| lowered.contains(term.as_str()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchFrame {
    Visit(u32),
    Exit { file_index: u32, path_length: usize },
}

impl CompactNtfsIndex {
    /// Searches the subtree below `root` (the root itself is not a
    /// candidate, as with the walker). Rows are emitted in batches of at
    /// most 256 rows and at most 1 Mi path code units; `is_cancelled` is
    /// polled before every entry.
    pub fn search_subtree(
        &self,
        root: FileReference,
        query: &SearchQuery,
        mut is_cancelled: impl FnMut() -> bool,
        mut on_batch: impl FnMut(Vec<SearchRow>),
    ) -> Result<SearchSummary> {
        let (root_index, root_meta) = self.file(root).ok_or_else(|| {
            NtfsError::new(
                ErrorKind::InvalidAttribute,
                root.record,
                "search root record not found",
            )
        })?;
        if !root_meta.is_directory() {
            return Err(NtfsError::new(
                ErrorKind::InvalidAttribute,
                root.record,
                "search root is not a directory",
            ));
        }

        let mut matcher = TextMatcher::new(&query.terms);
        let mut summary = SearchSummary::default();
        let mut ancestors = vec![false; self.files.len()];
        ancestors[root_index as usize] = true;
        let mut path: Vec<u16> = Vec::new();
        let mut stack: Vec<SearchFrame> = self
            .children_of(root_index)
            .iter()
            .rev()
            .map(|link| SearchFrame::Visit(*link))
            .collect();
        let mut batch: Vec<SearchRow> = Vec::new();
        let mut batch_units = 0usize;

        while let Some(frame) = stack.pop() {
            if is_cancelled() {
                return Err(NtfsError::cancelled("subtree search"));
            }
            let link_index = match frame {
                SearchFrame::Exit {
                    file_index,
                    path_length,
                } => {
                    ancestors[file_index as usize] = false;
                    path.truncate(path_length);
                    continue;
                }
                SearchFrame::Visit(link_index) => link_index,
            };
            let link = self.links[link_index as usize];
            let file = self.files[link.file_index as usize];
            let name = self.raw_name(link);
            if name.is_empty() {
                continue;
            }
            if !reportable_name(name) {
                summary.skipped_paths = summary.skipped_paths.saturating_add(1);
                continue;
            }
            let hidden = name[0] == DOT || file.is_hidden_attribute();
            if hidden && !query.include_hidden {
                continue;
            }
            summary.entries_visited = summary.entries_visited.saturating_add(1);

            let parent_length = path.len();
            if !path.is_empty() {
                path.push(PATH_SEPARATOR);
            }
            path.extend_from_slice(name);
            if path.len() > MAX_RELATIVE_PATH_UNITS {
                summary.skipped_paths = summary.skipped_paths.saturating_add(1);
                path.truncate(parent_length);
                continue;
            }

            let is_directory = file.is_directory();
            let opaque_reparse = is_directory
                && file.is_reparse_point()
                && self.children_of(link.file_index).is_empty();
            let package = is_package_component(name);
            let descend = is_directory && !opaque_reparse && (!package || query.descend_packages);

            let text_ok = matcher.matches_everything()
                || if query.match_path {
                    matcher.matches(&path)
                } else {
                    matcher.matches(name)
                };
            if text_ok {
                let kind = match (is_directory, file.is_reparse_point()) {
                    (true, true) => SearchEntryKind::ReparseDirectory,
                    (true, false) => SearchEntryKind::Directory,
                    (false, true) => SearchEntryKind::ReparseFile,
                    (false, false) => SearchEntryKind::File,
                };
                let (logical_bytes, allocated_bytes) = if is_directory {
                    (0, 0)
                } else {
                    (file.logical_bytes, file.allocated_bytes)
                };
                if batch.len() == MAX_BATCH_ROWS
                    || batch_units.saturating_add(path.len()) > SEARCH_BATCH_UNIT_BUDGET
                {
                    on_batch(std::mem::take(&mut batch));
                    batch_units = 0;
                }
                batch_units += path.len();
                batch.push(SearchRow {
                    relative_path: path.clone(),
                    file_record: file.reference(),
                    kind,
                    cloud: file.is_cloud_reparse(),
                    attributes: file.attributes
                        | if is_directory {
                            FILE_ATTRIBUTE_DIRECTORY
                        } else {
                            0
                        },
                    logical_bytes,
                    allocated_bytes,
                    modified_ticks: file.modified_ticks,
                    created_ticks: file.created_ticks,
                });
                summary.matches = summary.matches.saturating_add(1);
            }

            if descend {
                if ancestors[link.file_index as usize] {
                    summary.cycles_skipped = summary.cycles_skipped.saturating_add(1);
                    path.truncate(parent_length);
                    continue;
                }
                ancestors[link.file_index as usize] = true;
                summary.directories_descended = summary.directories_descended.saturating_add(1);
                stack.push(SearchFrame::Exit {
                    file_index: link.file_index,
                    path_length: parent_length,
                });
                stack.extend(
                    self.children_of(link.file_index)
                        .iter()
                        .rev()
                        .map(|child| SearchFrame::Visit(*child)),
                );
            } else {
                path.truncate(parent_length);
            }
        }
        if !batch.is_empty() {
            on_batch(batch);
        }
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::tests::{name, record, reference};
    use crate::{FileName, IndexBuilder, NameNamespace};

    fn raw(value: &str) -> Vec<u16> {
        value.encode_utf16().collect()
    }

    /// root(5)
    /// ├── Docs (10)            dir
    /// │   ├── Report.TXT (20)  also linked as root\hard-report.txt
    /// │   └── notes.md (21)    hidden attribute
    /// ├── .git (11)            dot dir → hidden
    /// │   └── config (22)
    /// ├── Tool.app (12)        package dir
    /// │   └── inner.txt (23)
    /// ├── junction (13)        reparse dir, no indexed children
    /// ├── OneDrive (14)        cloud reparse dir with children
    /// │   └── cloud-report.txt (24)
    /// ├── link.txt (30)        reparse file (symlink)
    /// └── "a\u{d800}b R" (31)  unpaired surrogate
    fn fixture() -> CompactNtfsIndex {
        let root = reference(5, 1);
        let docs = reference(10, 1);
        let git = reference(11, 1);
        let tool = reference(12, 1);
        let onedrive = reference(14, 1);
        let mut builder = IndexBuilder::default();
        builder.push(record(5, 1, true, false, vec![], 0)).unwrap();
        builder
            .push(record(10, 1, true, false, vec![name(root, "Docs")], 0))
            .unwrap();
        builder
            .push(record(11, 1, true, false, vec![name(root, ".git")], 0))
            .unwrap();
        builder
            .push(record(12, 1, true, false, vec![name(root, "Tool.app")], 0))
            .unwrap();
        builder
            .push(record(13, 1, true, true, vec![name(root, "junction")], 0))
            .unwrap();
        let mut cloud = record(14, 1, true, true, vec![name(root, "OneDrive")], 0);
        cloud.reparse_tag = Some(0x9000_601a);
        builder.push(cloud).unwrap();
        builder
            .push(record(
                20,
                1,
                false,
                false,
                vec![name(docs, "Report.TXT"), name(root, "hard-report.txt")],
                1000,
            ))
            .unwrap();
        let mut notes = record(21, 1, false, false, vec![name(docs, "notes.md")], 10);
        notes.file_attributes = Some(0x2);
        builder.push(notes).unwrap();
        builder
            .push(record(22, 1, false, false, vec![name(git, "config")], 10))
            .unwrap();
        builder
            .push(record(
                23,
                1,
                false,
                false,
                vec![name(tool, "inner.txt")],
                10,
            ))
            .unwrap();
        builder
            .push(record(
                24,
                1,
                false,
                false,
                vec![name(onedrive, "cloud-report.txt")],
                10,
            ))
            .unwrap();
        let mut symlink = record(30, 1, false, true, vec![name(root, "link.txt")], 0);
        symlink.reparse_tag = Some(0xa000_000c);
        builder.push(symlink).unwrap();
        builder
            .push(record(
                31,
                1,
                false,
                false,
                vec![FileName {
                    parent: root,
                    namespace: NameNamespace::Win32,
                    name: vec![b'a' as u16, 0xd800, b'b' as u16, b' ' as u16, b'R' as u16],
                    logical_bytes: 0,
                    allocated_bytes: 0,
                    file_attributes: 0,
                }],
                5,
            ))
            .unwrap();
        builder.finish().unwrap()
    }

    fn query(terms: &[&str]) -> SearchQuery {
        SearchQuery {
            terms: terms.iter().map(|term| term.to_string()).collect(),
            ..SearchQuery::default()
        }
    }

    fn run(index: &CompactNtfsIndex, query: &SearchQuery) -> (Vec<SearchRow>, SearchSummary) {
        let mut rows = Vec::new();
        let summary = index
            .search_subtree(reference(5, 1), query, || false, |batch| rows.extend(batch))
            .unwrap();
        (rows, summary)
    }

    fn paths(rows: &[SearchRow]) -> Vec<String> {
        let mut paths: Vec<String> = rows
            .iter()
            .map(|row| String::from_utf16_lossy(&row.relative_path))
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn name_match_is_case_insensitive_and_lists_every_hard_link() {
        let (rows, summary) = run(&fixture(), &query(&["report"]));
        assert_eq!(
            paths(&rows),
            vec![
                "Docs\\Report.TXT",
                "OneDrive\\cloud-report.txt",
                "hard-report.txt",
            ]
        );
        assert_eq!(summary.matches, 3);
        let report = rows
            .iter()
            .find(|row| row.display_name() == "Report.TXT")
            .unwrap();
        let hard = rows
            .iter()
            .find(|row| row.display_name() == "hard-report.txt")
            .unwrap();
        // Search lists every name with the file's full sizes: no
        // charge-once rule, unlike Disk Usage.
        assert_eq!(report.logical_bytes, 1000);
        assert_eq!(hard.logical_bytes, 1000);
        assert_eq!(report.file_record, hard.file_record);
        assert_eq!(report.created_ticks, 140);
        assert_eq!(report.modified_ticks, 200);
    }

    #[test]
    fn hidden_entries_are_neither_matched_nor_descended_unless_requested() {
        let index = fixture();
        let (rows, _) = run(&index, &query(&["o"]));
        let found = paths(&rows);
        assert!(!found.iter().any(|path| path.contains(".git")));
        assert!(!found.contains(&"Docs\\notes.md".to_string()));

        let mut with_hidden = query(&["config"]);
        with_hidden.include_hidden = true;
        let (rows, _) = run(&index, &with_hidden);
        assert_eq!(paths(&rows), vec![".git\\config"]);

        let mut notes = query(&["notes"]);
        notes.include_hidden = true;
        let (rows, _) = run(&index, &notes);
        assert!(rows[0].is_hidden());
        assert_eq!(rows[0].attributes & crate::FILE_ATTRIBUTE_HIDDEN, 2);
    }

    #[test]
    fn packages_match_by_name_and_are_descended_only_by_policy() {
        let index = fixture();
        let (rows, _) = run(&index, &query(&["t"]));
        let found = paths(&rows);
        assert!(found.contains(&"Tool.app".to_string()));
        assert!(!found.contains(&"Tool.app\\inner.txt".to_string()));

        let mut descend = query(&["inner"]);
        descend.descend_packages = true;
        let (rows, _) = run(&index, &descend);
        assert_eq!(paths(&rows), vec!["Tool.app\\inner.txt"]);
    }

    #[test]
    fn package_rule_follows_path_extension() {
        assert!(is_package_component(&raw("Foo.APP")));
        assert!(is_package_component(&raw("a.b.framework")));
        assert!(!is_package_component(&raw(".app")));
        assert!(!is_package_component(&raw("app")));
        assert!(!is_package_component(&raw("foo.app.zip")));
        assert!(!is_package_component(&[b'x' as u16, DOT, 0xd800]));
    }

    #[test]
    fn junction_is_listed_but_opaque_and_cloud_folder_is_descended() {
        let index = fixture();
        let (rows, _) = run(&index, &SearchQuery::default());
        let found = paths(&rows);
        let junction = rows
            .iter()
            .find(|row| row.display_name() == "junction")
            .unwrap();
        assert_eq!(junction.kind, SearchEntryKind::ReparseDirectory);
        assert!(!found.iter().any(|path| path.starts_with("junction\\")));

        let cloud = rows
            .iter()
            .find(|row| row.display_name() == "OneDrive")
            .unwrap();
        assert_eq!(cloud.kind, SearchEntryKind::ReparseDirectory);
        assert!(cloud.cloud);
        assert!(found.contains(&"OneDrive\\cloud-report.txt".to_string()));

        let link = rows
            .iter()
            .find(|row| row.display_name() == "link.txt")
            .unwrap();
        assert_eq!(link.kind, SearchEntryKind::ReparseFile);
        assert!(!link.cloud);

        let docs = rows
            .iter()
            .find(|row| row.display_name() == "Docs")
            .unwrap();
        assert_eq!(docs.kind, SearchEntryKind::Directory);
        assert_ne!(docs.attributes & FILE_ATTRIBUTE_DIRECTORY, 0);
        assert_eq!(docs.logical_bytes, 0);
    }

    #[test]
    fn empty_term_list_matches_every_visible_entry() {
        let (rows, summary) = run(&fixture(), &SearchQuery::default());
        // Docs, Report.TXT, Tool.app, junction, OneDrive, cloud-report.txt,
        // hard-report.txt, link.txt, the surrogate name. Hidden: .git,
        // config (under .git), notes.md.
        assert_eq!(rows.len(), 9);
        assert_eq!(summary.entries_visited, 9);
    }

    #[test]
    fn multi_term_query_is_an_and_of_substrings() {
        let index = fixture();
        let (rows, _) = run(&index, &query(&["report", "cloud"]));
        assert_eq!(paths(&rows), vec!["OneDrive\\cloud-report.txt"]);
        let (rows, _) = run(&index, &query(&["report", "nothing"]));
        assert!(rows.is_empty());
    }

    #[test]
    fn match_path_matches_the_backslash_joined_relative_path() {
        let index = fixture();
        let mut by_path = query(&["docs\\rep"]);
        by_path.match_path = true;
        let (rows, _) = run(&index, &by_path);
        assert_eq!(paths(&rows), vec!["Docs\\Report.TXT"]);

        // Without match_path the directory name is not part of the haystack.
        let (rows, _) = run(&index, &query(&["docs\\rep"]));
        assert!(rows.is_empty());

        // Every entry below Docs matches a term naming only the directory.
        let mut under = query(&["docs"]);
        under.match_path = true;
        let (rows, _) = run(&index, &under);
        assert_eq!(paths(&rows), vec!["Docs", "Docs\\Report.TXT"]);
    }

    #[test]
    fn unpaired_surrogate_is_preserved_and_matched_like_the_walker() {
        let index = fixture();
        // from_utf16_lossy turns the surrogate into U+FFFD, then lowercase.
        let (rows, _) = run(&index, &query(&["a\u{fffd}b r"]));
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].relative_path,
            vec![b'a' as u16, 0xd800, b'b' as u16, b' ' as u16, b'R' as u16]
        );
        // An ASCII-only term still matches through the slow path.
        let (rows, _) = run(&index, &query(&["b r"]));
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn non_ascii_case_folding_matches_to_lowercase() {
        let root = reference(5, 1);
        let mut builder = IndexBuilder::default();
        builder.push(record(5, 1, true, false, vec![], 0)).unwrap();
        builder
            .push(record(10, 1, false, false, vec![name(root, "ÉTÉ.txt")], 1))
            .unwrap();
        // KELVIN SIGN lowercases to ASCII 'k'.
        builder
            .push(record(
                11,
                1,
                false,
                false,
                vec![name(root, "\u{212a}ey")],
                1,
            ))
            .unwrap();
        builder
            .push(record(
                12,
                1,
                false,
                false,
                vec![name(root, "plain e.txt")],
                1,
            ))
            .unwrap();
        let index = builder.finish().unwrap();
        let (rows, _) = run(&index, &query(&["été"]));
        assert_eq!(rows.len(), 1);
        let (rows, _) = run(&index, &query(&["key"]));
        assert_eq!(rows.len(), 1);
        // A non-ASCII term never matches an ASCII-only name: only ÉTÉ.txt.
        let (rows, _) = run(&index, &query(&["\u{e9}"]));
        assert_eq!(paths(&rows), vec!["ÉTÉ.txt"]);
    }

    #[test]
    fn cancellation_mid_walk_stops_with_cancelled() {
        let index = fixture();
        let mut polls = 0;
        let mut emitted = 0usize;
        let error = index
            .search_subtree(
                reference(5, 1),
                &SearchQuery::default(),
                || {
                    polls += 1;
                    polls > 3
                },
                |batch| emitted += batch.len(),
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(emitted, 0);
    }

    #[test]
    fn batches_respect_row_cap() {
        let root = reference(5, 1);
        let mut builder = IndexBuilder::default();
        builder.push(record(5, 1, true, false, vec![], 0)).unwrap();
        for number in 0..600u64 {
            builder
                .push(record(
                    100 + number,
                    1,
                    false,
                    false,
                    vec![name(root, &format!("file-{number:04}"))],
                    1,
                ))
                .unwrap();
        }
        let index = builder.finish().unwrap();
        let mut sizes = Vec::new();
        index
            .search_subtree(
                root,
                &query(&["file"]),
                || false,
                |batch| sizes.push(batch.len()),
            )
            .unwrap();
        assert_eq!(sizes, vec![256, 256, 88]);
    }

    #[test]
    fn root_must_be_a_live_directory() {
        let index = fixture();
        assert!(index
            .search_subtree(reference(20, 1), &SearchQuery::default(), || false, |_| {})
            .is_err());
        assert!(index
            .search_subtree(reference(5, 2), &SearchQuery::default(), || false, |_| {})
            .is_err());
    }

    #[test]
    fn unreportable_names_are_skipped_not_fatal() {
        let root = reference(5, 1);
        let mut builder = IndexBuilder::default();
        builder.push(record(5, 1, true, false, vec![], 0)).unwrap();
        builder
            .push(record(10, 1, false, false, vec![name(root, "a/b")], 1))
            .unwrap();
        builder
            .push(record(11, 1, false, false, vec![name(root, "..")], 1))
            .unwrap();
        builder
            .push(record(12, 1, false, false, vec![name(root, "ok")], 1))
            .unwrap();
        let index = builder.finish().unwrap();
        let include = SearchQuery {
            include_hidden: true,
            ..SearchQuery::default()
        };
        let (rows, summary) = run(&index, &include);
        assert_eq!(paths(&rows), vec!["ok"]);
        assert_eq!(summary.skipped_paths, 2);
        assert!(rows
            .iter()
            .all(|row| crate::wire::valid_relative_path(&row.relative_path)));
    }

    #[test]
    fn tick_conversion_matches_walker_rule() {
        assert_eq!(ntfs_ticks_to_unix_seconds(0), None);
        assert_eq!(ntfs_ticks_to_unix_seconds(116_444_736_000_000_000), Some(0));
        assert_eq!(
            ntfs_ticks_to_unix_seconds(116_444_736_000_000_000 + 15_000_000),
            Some(1)
        );
        assert_eq!(ntfs_ticks_to_unix_seconds(116_444_735_990_000_000), None);
    }
}
