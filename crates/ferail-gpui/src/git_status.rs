//! Git awareness: the process-wide cache and scheduler behind the file
//! list's Git column and the status bar's branch label
//! (docs/features/GIT_STATUS.md).
//!
//! One `git status` result per repository root serves every tab and window
//! whose folder lies in that work tree, so moving between subfolders of a
//! repository costs no process at all while the result is fresh. Work starts
//! only from semantic events: a completed directory load, the watcher's
//! reload fan-out, Refresh, and the window coming back to the front. There
//! is no timer polling. Each repository runs at most one `git` child at a
//! time; a request arriving meanwhile is folded into a single rerun, and a
//! result overtaken by an invalidation is dropped by generation.
//!
//! Rendering never reaches this module: results are folded into each
//! table's row-parallel `git_marks` once per apply.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use ferail_core::git_status::{GitBranch, GitCell, GitTree, parse_porcelain_v2};
use ferail_core::node_store::normalize_path_key;
use ferail_fs_native::git::{self, GitError};
use gpui::{App, AsyncApp, Context};

use crate::process_state::ProcessState;
use crate::shell::Shell;

/// Coalesces a burst of requests (several tabs finishing their loads, a
/// watcher reload right after a navigation) into one `git` run.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// A result younger than this answers a navigation without asking `git`.
/// Changes the watcher reports and Refresh bypass it.
const FRESH_FOR: Duration = Duration::from_secs(30);
/// Coming back to the window re-reads a repository only when its result is
/// older than this, so flicking between apps does not spawn a child each
/// time.
const ACTIVATION_MIN_AGE: Duration = Duration::from_secs(2);
/// Upper bound on remembered folder → repository answers.
const DIR_MEMO_CAP: usize = 4096;

/// One repository's parsed state.
pub struct RepoSnapshot {
    pub branch: GitBranch,
    pub tree: GitTree,
}

/// What the status bar shows for a tab inside a work tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchLabel {
    /// Branch name, or the abbreviated commit when `detached`.
    pub name: String,
    pub detached: bool,
    pub upstream: Option<String>,
    pub ahead_behind: Option<(u32, u32)>,
}

impl BranchLabel {
    fn from_branch(branch: &GitBranch) -> Option<Self> {
        let (name, detached) = match (&branch.head, branch.short_oid()) {
            (Some(head), _) => (head.clone(), false),
            (None, Some(oid)) => (oid.to_string(), true),
            (None, None) => return None,
        };
        Some(Self {
            name,
            detached,
            upstream: branch.upstream.clone(),
            ahead_behind: branch.ahead_behind,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    /// Waiting out [`DEBOUNCE`] before spawning.
    Scheduled,
    Running,
}

struct RepoState {
    snapshot: Option<Rc<RepoSnapshot>>,
    fetched_at: Option<Instant>,
    /// Bumped by every invalidation; a run started under an older value is
    /// stale when it lands.
    generation: u64,
    phase: Phase,
    /// An invalidation arrived while a run was in flight.
    rerun: bool,
    stale: bool,
    /// The last run timed out or failed. Only an explicit Refresh asks
    /// again, so a repository too slow for the timeout is not re-spawned on
    /// every watcher event.
    gave_up: bool,
}

impl RepoState {
    fn new() -> Self {
        Self {
            snapshot: None,
            fetched_at: None,
            generation: 0,
            phase: Phase::Idle,
            rerun: false,
            stale: false,
            gave_up: false,
        }
    }

    fn wants_refresh(&self) -> bool {
        if self.gave_up {
            return false;
        }
        self.stale
            || self.snapshot.is_none()
            || self.fetched_at.is_none_or(|at| at.elapsed() >= FRESH_FOR)
    }

    fn invalidate(&mut self, explicit: bool) {
        self.stale = true;
        self.generation = self.generation.wrapping_add(1);
        if self.phase == Phase::Running {
            self.rerun = true;
        }
        if explicit {
            self.gave_up = false;
        }
    }
}

/// Where a folder stands, from memory alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// Inside the work tree rooted here.
    Root(PathBuf),
    NotRepo,
    /// Never asked: `git` has to answer.
    Unknown,
}

/// Process-wide state, owned by [`ProcessState::git`]. All keys are
/// `normalize_path_key` spellings.
#[derive(Default)]
pub struct GitCache {
    repos: HashMap<PathBuf, RepoState>,
    /// Folders whose repository (or lack of one) `git` has reported.
    dir_roots: HashMap<PathBuf, Option<PathBuf>>,
    discovering: HashSet<PathBuf>,
    /// No usable `git` binary: nothing is ever spawned again this session.
    git_missing: bool,
}

impl GitCache {
    pub fn lookup(&self, dir: &Path) -> Lookup {
        if self.git_missing {
            return Lookup::NotRepo;
        }
        if let Some(answer) = self.dir_roots.get(dir) {
            return match answer {
                Some(root) => Lookup::Root(root.clone()),
                None => Lookup::NotRepo,
            };
        }
        // The deepest known work tree containing the folder answers for it,
        // unless the folder sits where a nested repository could start.
        let Some(root) = self
            .repos
            .keys()
            .filter(|root| dir.starts_with(root))
            .max_by_key(|root| root.components().count())
        else {
            return Lookup::Unknown;
        };
        if let Some(snapshot) = &self.repos[root].snapshot
            && let Some(rel) = rel_dir(dir, root)
            && snapshot.tree.may_hold_nested_repo(&rel)
        {
            return Lookup::Unknown;
        }
        Lookup::Root(root.clone())
    }

    /// The snapshot covering `dir` and `dir` relative to its root.
    pub fn snapshot_for(&self, dir: &Path) -> Option<(Rc<RepoSnapshot>, String)> {
        let Lookup::Root(root) = self.lookup(dir) else {
            return None;
        };
        let snapshot = self.repos.get(&root)?.snapshot.clone()?;
        Some((snapshot, rel_dir(dir, &root)?))
    }

    /// Something changed at `paths`. Every repository containing one of
    /// them is re-read on its next request. `explicit` is the user's
    /// Refresh: it also forgets that a folder is not a repository (so a
    /// fresh `git init` is picked up) and retries a repository that timed
    /// out.
    pub fn invalidate(&mut self, paths: &[PathBuf], explicit: bool) {
        for path in paths {
            let key = normalize_path_key(path);
            if explicit {
                self.dir_roots.remove(&key);
            }
            for (root, state) in self.repos.iter_mut() {
                if key.starts_with(root) {
                    state.invalidate(explicit);
                }
            }
        }
    }

    fn remember_dir(&mut self, dir: PathBuf, root: Option<PathBuf>) {
        if self.dir_roots.len() >= DIR_MEMO_CAP {
            self.dir_roots.clear();
        }
        self.dir_roots.insert(dir, root);
    }

    fn forget_repo(&mut self, root: &Path) {
        self.repos.remove(root);
        self.dir_roots
            .retain(|_, answer| answer.as_deref() != Some(root));
        self.remember_dir(root.to_path_buf(), None);
    }
}

/// `dir` relative to `root`, `/`-separated, `""` for the root itself.
fn rel_dir(dir: &Path, root: &Path) -> Option<String> {
    let rel = dir.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// Ask for a repository's status unless a fresh result exists or a run is
/// already queued. Never blocks: the `git` child runs on the background
/// executor.
fn request_status(process: &Rc<ProcessState>, root: PathBuf, cx: &mut App) {
    {
        let mut cache = process.git.borrow_mut();
        if cache.git_missing {
            return;
        }
        let state = cache.repos.entry(root.clone()).or_insert_with(RepoState::new);
        // A run in flight already covers this request; an invalidation
        // since it started has set `rerun`.
        if state.phase != Phase::Idle || !state.wants_refresh() {
            return;
        }
        state.phase = Phase::Scheduled;
    }
    let process = process.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        cx.background_executor().timer(DEBOUNCE).await;
        let generation = {
            let mut cache = process.git.borrow_mut();
            let Some(state) = cache.repos.get_mut(&root) else {
                return;
            };
            state.phase = Phase::Running;
            state.rerun = false;
            state.stale = false;
            state.generation
        };
        let worker_root = root.clone();
        let result = cx
            .background_executor()
            .spawn(async move {
                git::status_porcelain(&worker_root).map(|bytes| parse_porcelain_v2(&bytes))
            })
            .await;
        let rerun = finish_status(&process, &root, generation, result);
        broadcast(&process, cx);
        if rerun {
            cx.update(|cx| request_status(&process, root, cx));
        }
    })
    .detach();
}

/// Store a finished run. Returns whether the repository must be read again.
fn finish_status(
    process: &Rc<ProcessState>,
    root: &Path,
    generation: u64,
    result: Result<ferail_core::git_status::GitStatus, GitError>,
) -> bool {
    let mut cache = process.git.borrow_mut();
    let Some(state) = cache.repos.get_mut(root) else {
        return false;
    };
    state.phase = Phase::Idle;
    let rerun = std::mem::take(&mut state.rerun);
    match result {
        Ok(status) => {
            // A result overtaken by an invalidation is dropped, except when
            // there is nothing at all to show yet: then it is better than an
            // empty column while the rerun catches up.
            if state.generation == generation || state.snapshot.is_none() {
                state.snapshot = Some(Rc::new(RepoSnapshot {
                    tree: GitTree::build(&status.entries),
                    branch: status.branch,
                }));
                state.fetched_at = Some(Instant::now());
                state.gave_up = false;
            }
            rerun || state.generation != generation
        }
        Err(GitError::NotARepo) => {
            cache.forget_repo(root);
            false
        }
        Err(GitError::Missing) => {
            cache.git_missing = true;
            cache.repos.clear();
            false
        }
        Err(err @ (GitError::TimedOut | GitError::Failed)) => {
            crate::log_info!(90, "git status gave up on a repository: {err:?}");
            state.snapshot = None;
            state.fetched_at = Some(Instant::now());
            state.gave_up = true;
            false
        }
    }
}

/// Find the work tree containing `dir` (one `git rev-parse`), then read it.
fn request_discovery(process: &Rc<ProcessState>, dir: PathBuf, cx: &mut App) {
    {
        let mut cache = process.git.borrow_mut();
        if cache.git_missing || !cache.discovering.insert(dir.clone()) {
            return;
        }
    }
    let process = process.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let worker_dir = dir.clone();
        let found = cx
            .background_executor()
            .spawn(async move { git::discover_root(&worker_dir) })
            .await;
        let root = {
            let mut cache = process.git.borrow_mut();
            cache.discovering.remove(&dir);
            match found {
                Ok(root) => {
                    let root = normalize_path_key(&root);
                    cache.remember_dir(dir, Some(root.clone()));
                    cache
                        .repos
                        .entry(root.clone())
                        .or_insert_with(RepoState::new);
                    Some(root)
                }
                Err(GitError::Missing) => {
                    cache.git_missing = true;
                    None
                }
                // Not a work tree, or `git` too slow to say: remember the
                // answer so this folder never spawns again until Refresh.
                Err(_) => {
                    cache.remember_dir(dir, None);
                    None
                }
            }
        };
        match root {
            Some(root) => {
                // A cached result may already cover it (a sibling tab got
                // there first): show it now, read again only if stale.
                broadcast(&process, cx);
                cx.update(|cx| request_status(&process, root, cx));
            }
            None => broadcast(&process, cx),
        }
    })
    .detach();
}

/// Re-apply cached results to every tab of every window.
fn broadcast(process: &Rc<ProcessState>, cx: &mut AsyncApp) {
    for weak in process.live_shells() {
        if let Some(shell) = weak.upgrade() {
            shell.update(cx, |this, cx| this.apply_git_to_tabs(cx));
        }
    }
}

/// Record a change the watcher or a file operation reported.
pub(crate) fn invalidate(process: &ProcessState, paths: &[PathBuf], explicit: bool) {
    process.git.borrow_mut().invalidate(paths, explicit);
}

impl Shell {
    /// Whether tab `idx` shows a plain directory listing Git can describe.
    fn git_dir_for_tab(&self, idx: usize) -> Option<PathBuf> {
        let tab = self.tabs.get(idx)?;
        if crate::safe_mode::enabled()
            || tab.tool_result.is_some()
            || tab.platform_namespace.is_some()
        {
            return None;
        }
        Some(normalize_path_key(&tab.current_dir))
    }

    /// A directory load finished in tab `idx`: show what the cache knows at
    /// once, and ask `git` only when the answer is missing or stale.
    pub(crate) fn refresh_git_in_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.apply_git_to_tab(idx, cx);
        let Some(dir) = self.git_dir_for_tab(idx) else {
            return;
        };
        let lookup = self.process.git.borrow().lookup(&dir);
        let process = self.process.clone();
        match lookup {
            Lookup::NotRepo => {}
            Lookup::Root(root) => request_status(&process, root, cx),
            Lookup::Unknown => request_discovery(&process, dir, cx),
        }
    }

    /// The window came back to the front: the user may have committed,
    /// staged or switched branch elsewhere. Re-read the repositories this
    /// window shows, unless they were read moments ago.
    pub(crate) fn refresh_git_on_activation(&mut self, cx: &mut Context<Self>) {
        let roots: HashSet<PathBuf> = (0..self.tabs.len())
            .filter_map(|idx| self.git_dir_for_tab(idx))
            .filter_map(|dir| match self.process.git.borrow().lookup(&dir) {
                Lookup::Root(root) => Some(root),
                _ => None,
            })
            .collect();
        let process = self.process.clone();
        for root in roots {
            {
                let mut cache = process.git.borrow_mut();
                let Some(state) = cache.repos.get_mut(&root) else {
                    continue;
                };
                if state
                    .fetched_at
                    .is_some_and(|at| at.elapsed() < ACTIVATION_MIN_AGE)
                {
                    continue;
                }
                state.invalidate(false);
            }
            request_status(&process, root, cx);
        }
    }

    pub(crate) fn apply_git_to_tabs(&mut self, cx: &mut Context<Self>) {
        for idx in 0..self.tabs.len() {
            self.apply_git_to_tab(idx, cx);
        }
    }

    /// Fold the cached snapshot into tab `idx`'s row marks and branch label.
    /// O(rows) with one hash probe per row; runs once per result or load,
    /// never per frame.
    pub(crate) fn apply_git_to_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        let found = self
            .git_dir_for_tab(idx)
            .and_then(|dir| self.process.git.borrow().snapshot_for(&dir));
        let branch = found
            .as_ref()
            .and_then(|(snapshot, _)| BranchLabel::from_branch(&snapshot.branch));
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        let branch_changed = tab.git_branch != branch;
        tab.git_branch = branch;
        let table = tab.table.clone();
        table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            let marks: Vec<GitCell> = match &found {
                Some((snapshot, rel)) => {
                    let view = snapshot.tree.dir_view(rel);
                    if view.is_clean() {
                        Vec::new()
                    } else {
                        delegate
                            .entries
                            .iter()
                            .map(|entry| view.cell(entry.name.as_ref()))
                            .collect()
                    }
                }
                None => Vec::new(),
            };
            // A clean listing keeps the vector empty: zero bytes per row.
            let marks = if marks.iter().all(|cell| cell.is_clean()) {
                Vec::new()
            } else {
                marks
            };
            if delegate.git_marks != marks {
                delegate.git_marks = marks;
                state.refresh(cx);
            }
        });
        if branch_changed {
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(records: &[u8]) -> Rc<RepoSnapshot> {
        let status = parse_porcelain_v2(records);
        Rc::new(RepoSnapshot {
            tree: GitTree::build(&status.entries),
            branch: status.branch,
        })
    }

    fn cache_with_repo(root: &str, snap: Option<Rc<RepoSnapshot>>) -> GitCache {
        let mut cache = GitCache::default();
        let mut state = RepoState::new();
        state.fetched_at = snap.as_ref().map(|_| Instant::now());
        state.snapshot = snap;
        cache.repos.insert(PathBuf::from(root), state);
        cache
    }

    #[test]
    fn subfolders_reuse_the_repository_result() {
        let cache = cache_with_repo("/r", Some(snapshot(b"? src/new.rs\0")));
        assert_eq!(cache.lookup(Path::new("/r/src/deep")), Lookup::Root("/r".into()));
        assert_eq!(cache.lookup(Path::new("/elsewhere")), Lookup::Unknown);
        let (snap, rel) = cache.snapshot_for(Path::new("/r/src")).unwrap();
        assert_eq!(rel, "src");
        assert_eq!(
            snap.tree.dir_view(&rel).cell("new.rs").mark,
            ferail_core::git_status::GitMark::Untracked
        );
    }

    #[test]
    fn untracked_subtrees_are_asked_again() {
        let cache = cache_with_repo("/r", Some(snapshot(b"? vendor/\0")));
        assert_eq!(cache.lookup(Path::new("/r/vendor/nested")), Lookup::Unknown);
    }

    #[test]
    fn remembered_answers_win_and_refresh_forgets_negatives() {
        let mut cache = cache_with_repo("/r", None);
        cache.remember_dir(PathBuf::from("/plain"), None);
        assert_eq!(cache.lookup(Path::new("/plain")), Lookup::NotRepo);
        cache.invalidate(&[PathBuf::from("/plain")], false);
        assert_eq!(cache.lookup(Path::new("/plain")), Lookup::NotRepo);
        cache.invalidate(&[PathBuf::from("/plain")], true);
        assert_eq!(cache.lookup(Path::new("/plain")), Lookup::Unknown);
    }

    #[test]
    fn freshness_and_invalidation() {
        let mut cache = cache_with_repo("/r", Some(snapshot(b"")));
        assert!(!cache.repos[Path::new("/r")].wants_refresh());
        cache.invalidate(&[PathBuf::from("/r/a/b")], false);
        let state = &cache.repos[Path::new("/r")];
        assert!(state.wants_refresh());
        assert_eq!(state.generation, 1);
        // A change outside the work tree leaves it alone.
        cache.invalidate(&[PathBuf::from("/other")], false);
        assert_eq!(cache.repos[Path::new("/r")].generation, 1);
    }

    #[test]
    fn a_timed_out_repository_waits_for_refresh() {
        let mut cache = cache_with_repo("/r", None);
        cache.repos.get_mut(Path::new("/r")).unwrap().gave_up = true;
        cache.invalidate(&[PathBuf::from("/r")], false);
        assert!(!cache.repos[Path::new("/r")].wants_refresh());
        cache.invalidate(&[PathBuf::from("/r")], true);
        assert!(cache.repos[Path::new("/r")].wants_refresh());
    }

    #[test]
    fn invalidation_during_a_run_queues_one_rerun() {
        let mut cache = cache_with_repo("/r", None);
        cache.repos.get_mut(Path::new("/r")).unwrap().phase = Phase::Running;
        cache.invalidate(&[PathBuf::from("/r/x")], false);
        cache.invalidate(&[PathBuf::from("/r/y")], false);
        let state = &cache.repos[Path::new("/r")];
        assert!(state.rerun);
        assert_eq!(state.generation, 2);
    }

    #[test]
    fn branch_label_prefers_the_branch_name() {
        let named = GitBranch {
            head: Some("main".into()),
            oid: Some("0123456789".into()),
            upstream: Some("origin/main".into()),
            ahead_behind: Some((1, 0)),
        };
        let label = BranchLabel::from_branch(&named).unwrap();
        assert_eq!(label.name, "main");
        assert!(!label.detached);
        let detached = GitBranch {
            head: None,
            oid: Some("0123456789".into()),
            ..Default::default()
        };
        let label = BranchLabel::from_branch(&detached).unwrap();
        assert_eq!(label.name, "0123456");
        assert!(label.detached);
        assert!(BranchLabel::from_branch(&GitBranch::default()).is_none());
    }

    #[test]
    fn rel_dir_is_slash_separated() {
        assert_eq!(rel_dir(Path::new("/r"), Path::new("/r")).as_deref(), Some(""));
        assert_eq!(
            rel_dir(Path::new("/r/a/b"), Path::new("/r")).as_deref(),
            Some("a/b")
        );
        assert_eq!(rel_dir(Path::new("/x"), Path::new("/r")), None);
    }
}
