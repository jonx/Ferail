//! Recursive subtree search worker (Tier 1 of [docs/features/SEARCH.md]).
//!
//! Pure-function, mirroring the shape of
//! [`NativeFs::scan_disk_usage`] and [`NativeFs::enumerate_streaming`]:
//! same DFS stack, same `batch_size`, same `AtomicBool` cancel checked
//! between dirents and level boundaries, same throttled `on_progress`.
//! The host owns the thread spawn + event-loop dispatch.
//!
//! Matches are emitted as [`SearchHit`]s — a [`FileEntry`] (ready to drop
//! into the file list) plus its absolute path (search results live
//! outside the current directory, so the host must register the path in
//! its own node store). The walker reuses the same `FileEntry` builder
//! as streaming enumeration, so a hit row renders identically to a
//! normal listing row.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use ferail_core::{EnumerationError, FileEntry, NodeId};

use crate::disk_usage_scanner::{is_icloud_path, is_mac_package};
use crate::{map_io_error, NativeFs};

/// Default match-batch size, mirroring `DEFAULT_ENUMERATION_BATCH`.
pub const DEFAULT_SEARCH_BATCH: usize = 256;
/// Minimum gap between progress callbacks.
const PROGRESS_THROTTLE_MS: u128 = 250;

/// What to look for. Substring matching only this iter; glob / regex
/// can layer on later behind the same struct.
#[derive(Clone, Debug)]
pub struct SearchQuery {
    /// Case-insensitive needle. Empty matches nothing (callers should
    /// avoid spawning a walk for an empty query). Ignored when `expr`
    /// is set.
    pub needle: String,
    /// Match against the path relative to the search root instead of
    /// the file name alone.
    pub match_path: bool,
    /// Descend into (and match) dot-files / `UF_HIDDEN` entries.
    pub include_hidden: bool,
    /// Structured query (`size:>10mb mod:week …`). When set it replaces
    /// `needle`: its text terms match the name/path haystack, its
    /// metadata terms test the built row's cached fields. An
    /// expression with only metadata terms matches every name — the
    /// walk is then a pure metadata scan.
    pub expr: Option<ferail_core::filter_expr::FilterExpr>,
}

/// A single search result: a ready-to-display row plus its absolute
/// path, since the row lives outside the currently-loaded directory.
#[derive(Clone, Debug)]
pub struct SearchHit {
    pub entry: FileEntry,
    pub path: PathBuf,
}

/// Running totals for the status bar.
#[derive(Clone, Copy, Debug, Default)]
pub struct SearchStats {
    pub dirs_scanned: u64,
    pub matches: u64,
}

impl NativeFs {
    /// Recursive depth-first search of `root`. Emits [`SearchHit`]s in
    /// batches of up to `batch_size`, calling `on_batch` whenever the
    /// buffer fills or the walk finishes. `on_progress` fires at most
    /// once per ~250 ms with running totals.
    ///
    /// `cancel` is checked between dirents and at each level boundary;
    /// when set, the in-flight buffer is flushed and the walk returns
    /// without touching the rest of the tree.
    ///
    /// Mac-safe, matching the disk-usage walker:
    /// - dataless / iCloud placeholders are skipped (never downloaded);
    /// - `descend_packages = false` treats `*.app` / `*.bundle` etc. as
    ///   opaque leaves — they can match by name but are not descended;
    /// - symlinks are inspected via `symlink_metadata` and never
    ///   followed, keeping the walk cycle-safe;
    /// - per-directory read failures are absorbed (partial-but-complete).
    // The subtree walker genuinely needs each of these inputs.
    #[allow(clippy::too_many_arguments)]
    pub fn search_subtree(
        &self,
        root: &Path,
        query: &SearchQuery,
        batch_size: usize,
        cancel: &AtomicBool,
        descend_packages: bool,
        on_batch: impl FnMut(Vec<SearchHit>),
        mut on_progress: impl FnMut(SearchStats),
    ) -> Option<EnumerationError> {
        let needle = query.needle.to_lowercase();
        let expr = query.expr.as_ref();
        let empty_query = match expr {
            Some(e) => e.is_empty(),
            None => needle.is_empty(),
        };
        if empty_query {
            on_progress(SearchStats::default());
            return None;
        }
        self.walk_subtree(
            root,
            Some(query),
            batch_size,
            cancel,
            descend_packages,
            query.include_hidden,
            None,
            on_batch,
            on_progress,
        )
    }

    /// Recursively enumerate every item below `root`, assigning sequential
    /// scan-local NodeIds beginning above `id_base`. Unlike
    /// [`Self::search_subtree`], this never registers result paths in
    /// NativeFs's process-lifetime identity maps.
    #[allow(clippy::too_many_arguments)]
    pub fn flat_subtree(
        &self,
        root: &Path,
        include_hidden: bool,
        include_directories: bool,
        batch_size: usize,
        cancel: &AtomicBool,
        descend_packages: bool,
        id_base: u64,
        on_batch: impl FnMut(Vec<SearchHit>),
        on_progress: impl FnMut(SearchStats),
    ) -> Option<EnumerationError> {
        self.walk_subtree(
            root,
            None,
            batch_size,
            cancel,
            descend_packages,
            include_hidden,
            Some((id_base, include_directories)),
            on_batch,
            on_progress,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_subtree(
        &self,
        root: &Path,
        query: Option<&SearchQuery>,
        batch_size: usize,
        cancel: &AtomicBool,
        descend_packages: bool,
        include_hidden: bool,
        local_identity: Option<(u64, bool)>,
        mut on_batch: impl FnMut(Vec<SearchHit>),
        mut on_progress: impl FnMut(SearchStats),
    ) -> Option<EnumerationError> {
        let canonical_root = match fs::canonicalize(root) {
            Ok(p) => p,
            Err(e) => return Some(map_io_error(&e)),
        };
        match fs::read_dir(&canonical_root) {
            Ok(rd) => drop(rd),
            Err(e) => return Some(map_io_error(&e)),
        }

        let needle = query.map(|query| query.needle.to_lowercase());
        let expr = query.and_then(|query| query.expr.as_ref());

        let mut buffer: Vec<SearchHit> = Vec::with_capacity(batch_size);
        let mut stats = SearchStats::default();
        let mut last_progress = Instant::now();
        let workers = crate::directory_reader::recommended_recursive_workers(&canonical_root);

        crate::directory_reader::walk(canonical_root.clone(), cancel, workers, |event| {
            use crate::directory_reader::DirectoryWalkEvent;
            let DirectoryWalkEvent::Batch(_directory, entries) = event else {
                if matches!(event, DirectoryWalkEvent::Started(_)) {
                    stats.dirs_scanned = stats.dirs_scanned.saturating_add(1);
                }
                if last_progress.elapsed().as_millis() >= PROGRESS_THROTTLE_MS {
                    on_progress(stats);
                    last_progress = Instant::now();
                }
                return Vec::new();
            };
            let mut children = Vec::new();
            for entry in entries {
                let child_path = &entry.path;
                let name = entry.name.as_str();

                // Hidden filter: gate both matching and descent so a
                // hidden tree doesn't get walked when not requested.
                if !include_hidden && entry.hidden {
                    continue;
                }
                // Never download an iCloud placeholder just to search it.
                if is_icloud_path(child_path) && is_dataless_flags(entry.flags) {
                    continue;
                }

                let is_symlink = entry.is_symlink();
                let mac_pkg = is_mac_package(child_path);
                let is_dir = entry.is_dir() && !is_symlink;
                let descend = is_dir && (!mac_pkg || descend_packages);

                // Test the match against name or relative path.
                let text_ok = match query {
                    None => true,
                    Some(query) => {
                        let haystack = if query.match_path {
                            child_path
                                .strip_prefix(&canonical_root)
                                .unwrap_or(child_path)
                                .to_string_lossy()
                                .to_lowercase()
                        } else {
                            name.to_lowercase()
                        };
                        match expr {
                            Some(e) => e.text_matches(&haystack),
                            None => haystack.contains(needle.as_deref().unwrap_or_default()),
                        }
                    }
                };
                let include_directories = local_identity.is_none_or(|(_, include)| include);
                let emit = text_ok && (include_directories || !is_dir || mac_pkg);
                if emit {
                    let id = local_identity.and_then(|(base, _)| {
                        NodeId::from_raw(base.saturating_add(stats.matches).saturating_add(1))
                    });
                    let id = id.unwrap_or_else(|| self.id_for_path(child_path));
                    let row = self.file_entry_from_directory_entry_with_id(&entry, id);
                    if expr.is_none_or(|e| e.metadata_matches(&row)) {
                        buffer.push(SearchHit {
                            entry: row,
                            path: child_path.clone(),
                        });
                        stats.matches = stats.matches.saturating_add(1);
                        if buffer.len() >= batch_size {
                            on_batch(std::mem::take(&mut buffer));
                            buffer.reserve(batch_size);
                        }
                    }
                }

                if descend {
                    children.push(entry.path);
                }
            }
            if last_progress.elapsed().as_millis() >= PROGRESS_THROTTLE_MS {
                on_progress(stats);
                last_progress = Instant::now();
            }
            children
        });

        if !buffer.is_empty() {
            on_batch(buffer);
        }
        on_progress(stats);
        None
    }
}

/// True when the file is an undownloaded cloud placeholder (APFS
/// dataless / `SF_DATALESS`). Reading it would trigger a network
/// download — the prime directive forbids that off a semantic event.
#[cfg(target_os = "macos")]
fn is_dataless_flags(flags: u32) -> bool {
    // <sys/stat.h>: SF_DATALESS — "file is dataless object" (the
    // placeholder for a not-yet-materialized iCloud / FileProvider file).
    const SF_DATALESS: u32 = 0x4000_0000;
    flags & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
fn is_dataless_flags(_flags: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::sync::atomic::AtomicU32;
    use std::sync::atomic::Ordering;

    /// Self-cleaning unique temp dir (the crate has no `tempfile` dep;
    /// the disk-usage tests roll their own too).
    struct Fixture {
        root: PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    impl Fixture {
        fn path(&self) -> &Path {
            &self.root
        }
    }

    fn fixture() -> Fixture {
        // tmp/{readme.txt, notes.md, sub/{report.txt, image.png}, .hidden/secret.txt}
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("ferail-searchfix-{}-{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        File::create(root.join("readme.txt"))
            .unwrap()
            .write_all(b"x")
            .unwrap();
        File::create(root.join("notes.md"))
            .unwrap()
            .write_all(b"x")
            .unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        File::create(root.join("sub").join("report.txt"))
            .unwrap()
            .write_all(b"x")
            .unwrap();
        File::create(root.join("sub").join("image.png"))
            .unwrap()
            .write_all(b"x")
            .unwrap();
        fs::create_dir(root.join(".hidden")).unwrap();
        File::create(root.join(".hidden").join("secret.txt"))
            .unwrap()
            .write_all(b"x")
            .unwrap();
        Fixture { root }
    }

    fn run(root: &Path, query: SearchQuery) -> Vec<String> {
        let fs = NativeFs::new();
        let cancel = AtomicBool::new(false);
        let mut hits: Vec<String> = Vec::new();
        fs.search_subtree(
            root,
            &query,
            8,
            &cancel,
            false,
            |batch| hits.extend(batch.into_iter().map(|h| h.entry.name.to_string())),
            |_| {},
        );
        hits.sort();
        hits
    }

    #[test]
    fn name_substring_matches_recursively() {
        let tmp = fixture();
        let hits = run(
            tmp.path(),
            SearchQuery {
                needle: "txt".into(),
                match_path: false,
                include_hidden: false,
                expr: None,
            },
        );
        // readme.txt + sub/report.txt; .hidden/secret.txt excluded.
        assert_eq!(hits, vec!["readme.txt", "report.txt"]);
    }

    #[test]
    fn case_insensitive() {
        let tmp = fixture();
        let hits = run(
            tmp.path(),
            SearchQuery {
                needle: "REPORT".into(),
                match_path: false,
                include_hidden: false,
                expr: None,
            },
        );
        assert_eq!(hits, vec!["report.txt"]);
    }

    #[test]
    fn include_hidden_reaches_dot_trees() {
        let tmp = fixture();
        let hits = run(
            tmp.path(),
            SearchQuery {
                needle: "secret".into(),
                match_path: false,
                include_hidden: true,
                expr: None,
            },
        );
        assert_eq!(hits, vec!["secret.txt"]);
    }

    #[test]
    fn empty_needle_yields_nothing() {
        let tmp = fixture();
        let hits = run(
            tmp.path(),
            SearchQuery {
                needle: String::new(),
                match_path: false,
                include_hidden: false,
                expr: None,
            },
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn cancel_stops_walk() {
        let tmp = fixture();
        let fs = NativeFs::new();
        let cancel = AtomicBool::new(true); // pre-cancelled
        let mut count = 0;
        let err = fs.search_subtree(
            tmp.path(),
            &SearchQuery {
                needle: "txt".into(),
                match_path: false,
                include_hidden: false,
                expr: None,
            },
            8,
            &cancel,
            false,
            |batch| count += batch.len(),
            |_| {},
        );
        assert!(err.is_none());
        assert_eq!(count, 0);
    }

    #[test]
    fn flat_walk_is_files_only_and_uses_scan_local_identity() {
        let tmp = fixture();
        let fs = NativeFs::new();
        let cancel = AtomicBool::new(false);
        let base = 1_u64 << 60;
        let mut hits = Vec::new();
        let mut final_stats = SearchStats::default();
        let err = fs.flat_subtree(
            tmp.path(),
            false,
            false,
            2,
            &cancel,
            false,
            base,
            |batch| hits.extend(batch),
            |stats| final_stats = stats,
        );
        assert!(err.is_none());
        assert_eq!(hits.len(), 4);
        assert_eq!(final_stats.matches, 4);
        assert!(hits
            .iter()
            .all(|hit| !matches!(hit.entry.kind, ferail_core::EntryKind::Directory)));
        assert!(hits.iter().all(|hit| hit.entry.id.as_raw() > base));
        assert!(hits.iter().all(|hit| fs.path_for(hit.entry.id).is_none()));
        assert!(hits
            .iter()
            .all(|hit| !hit.path.starts_with(tmp.path().join(".hidden"))));
    }

    #[test]
    fn flat_walk_can_include_hidden_trees() {
        let tmp = fixture();
        let fs = NativeFs::new();
        let cancel = AtomicBool::new(false);
        let mut names = Vec::new();
        fs.flat_subtree(
            tmp.path(),
            true,
            false,
            8,
            &cancel,
            false,
            1_u64 << 60,
            |batch| names.extend(batch.into_iter().map(|hit| hit.entry.name)),
            |_| {},
        );
        names.sort();
        assert!(names.iter().any(|name| name.as_ref() == "secret.txt"));
    }
}
