//! Read-only `git` invocations for the Git column
//! (docs/features/GIT_STATUS.md).
//!
//! Ferail links no Git library: it runs the user's own `git` binary, and
//! only ever two read-only commands (`rev-parse` to find the work tree,
//! `status` to read it). Every call blocks until the child exits or
//! [`GIT_TIMEOUT`] passes, so both entry points assert they are off the UI
//! thread.
//!
//! The child can never wait on the user: stdin is closed, terminal prompts
//! and pagers are disabled, optional index writes are off
//! (`GIT_OPTIONAL_LOCKS=0`, so Ferail never touches `.git/index`), and the
//! fsmonitor hook is not consulted. Inherited `GIT_DIR`-style variables are
//! dropped so a launch from inside a shell session cannot point the query at
//! another repository.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

/// How long one `git` call may take before the child is killed. A huge
/// repository on a slow disk can exceed it; the column then stays empty for
/// that repository until something invalidates it.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Output beyond this is treated as a failure rather than parsed partially.
const MAX_OUTPUT: u64 = 64 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitError {
    /// No usable `git` binary on this machine.
    Missing,
    /// `git` ran and said no (not a work tree, unsafe ownership, …).
    NotARepo,
    TimedOut,
    /// Spawn or pipe failure, or oversized output.
    Failed,
}

/// The work-tree root containing `dir`, spelled from `dir` itself (`dir`
/// joined with as many `..` as `git rev-parse --show-cdup` reports), so it
/// keeps the caller's spelling of symlinked prefixes such as `/tmp`.
pub fn discover_root(dir: &Path) -> Result<PathBuf, GitError> {
    ferail_core::path_guard::assert_off_ui_thread("git::discover_root");
    let out = run(dir, &["rev-parse", "--is-inside-work-tree", "--show-cdup"])?;
    let text = String::from_utf8_lossy(&out);
    let mut lines = text.lines();
    if lines.next() != Some("true") {
        return Err(GitError::NotARepo);
    }
    Ok(root_from_cdup(dir, lines.next().unwrap_or("")))
}

/// Raw `git status --porcelain=v2 -z --branch` output for the work tree at
/// `root`, paths relative to `root`. Parse it with
/// `ferail_core::git_status::parse_porcelain_v2`.
///
/// `--untracked-files=normal` reports a new folder once instead of every
/// file in it, and `--ignored=matching` does the same for ignored folders,
/// so the output stays small even beside a `target/` or `node_modules/`.
pub fn status_porcelain(root: &Path) -> Result<Vec<u8>, GitError> {
    ferail_core::path_guard::assert_off_ui_thread("git::status_porcelain");
    run(
        root,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=normal",
            "--ignored=matching",
            "--find-renames",
        ],
    )
}

fn root_from_cdup(dir: &Path, cdup: &str) -> PathBuf {
    let ups = cdup.split('/').filter(|part| *part == "..").count();
    let mut root = dir.to_path_buf();
    for _ in 0..ups {
        if !root.pop() {
            break;
        }
    }
    root
}

#[cfg(target_os = "aros")]
fn run(_cwd: &Path, _args: &[&str]) -> Result<Vec<u8>, GitError> {
    Err(GitError::Missing)
}

#[cfg(not(target_os = "aros"))]
fn run(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};

    let git = git_binary().ok_or(GitError::Missing)?;
    let mut cmd = Command::new(git);
    cmd.arg("-C")
        .arg(cwd)
        .args(["--no-pager", "-c", "core.fsmonitor=false"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_PREFIX")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => GitError::Missing,
        _ => GitError::Failed,
    })?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(GitError::Failed);
    };
    // The pipe drains on its own thread so a large output can never fill
    // the pipe buffer and stall the child while we wait on the deadline.
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("git-output".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let ok = stdout.take(MAX_OUTPUT + 1).read_to_end(&mut buf).is_ok();
            let _ = tx.send(ok.then_some(buf));
        });
    if reader.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(GitError::Failed);
    }
    match rx.recv_timeout(GIT_TIMEOUT) {
        Ok(Some(buf)) => {
            let status = child.wait().map_err(|_| GitError::Failed)?;
            if buf.len() as u64 > MAX_OUTPUT {
                return Err(GitError::Failed);
            }
            if status.success() {
                Ok(buf)
            } else {
                Err(GitError::NotARepo)
            }
        }
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(GitError::Failed)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(GitError::TimedOut)
        }
    }
}

/// Resolved once per process: stat calls only, on whichever worker asks
/// first.
fn git_binary() -> Option<&'static Path> {
    static GIT: OnceLock<Option<PathBuf>> = OnceLock::new();
    GIT.get_or_init(find_git).as_deref()
}

fn find_git() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "git.exe" } else { "git" };
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).map(|dir| dir.join(exe)).collect())
        .unwrap_or_default();
    // An app started from Finder gets a minimal PATH; look where package
    // managers put git too.
    if cfg!(target_os = "macos") {
        candidates.push(PathBuf::from("/opt/homebrew/bin/git"));
        candidates.push(PathBuf::from("/usr/local/bin/git"));
        candidates.push(PathBuf::from("/usr/bin/git"));
    } else if cfg!(unix) {
        candidates.push(PathBuf::from("/usr/bin/git"));
        candidates.push(PathBuf::from("/usr/local/bin/git"));
    }
    candidates.into_iter().find(|candidate| {
        candidate.is_absolute()
            && candidate.components().all(|c| !matches!(c, Component::ParentDir))
            && candidate.is_file()
            && usable(candidate)
    })
}

/// On macOS `/usr/bin/git` is a stub: without the developer tools installed
/// it opens an "install the command line developer tools" dialog instead of
/// running. Accept it only when a real git sits behind it.
#[cfg(target_os = "macos")]
fn usable(candidate: &Path) -> bool {
    if candidate != Path::new("/usr/bin/git") {
        return true;
    }
    let developer_dir = std::env::var_os("DEVELOPER_DIR").map(PathBuf::from);
    developer_dir
        .into_iter()
        .chain([
            PathBuf::from("/var/db/xcode_select_link"),
            PathBuf::from("/Library/Developer/CommandLineTools"),
        ])
        .any(|dir| dir.join("usr/bin/git").is_file())
}

#[cfg(not(target_os = "macos"))]
fn usable(_candidate: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdup_climbs_lexically() {
        let dir = Path::new("/tmp/repo/a/b");
        assert_eq!(root_from_cdup(dir, ""), PathBuf::from("/tmp/repo/a/b"));
        assert_eq!(root_from_cdup(dir, "../"), PathBuf::from("/tmp/repo/a"));
        assert_eq!(root_from_cdup(dir, "../../"), PathBuf::from("/tmp/repo"));
        assert_eq!(root_from_cdup(Path::new("/a"), "../../../"), PathBuf::from("/"));
    }
}
