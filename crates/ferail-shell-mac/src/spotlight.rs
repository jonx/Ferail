//! Spotlight-backed global search (Tier 2 of [docs/features/SEARCH.md]).
//!
//! macOS already maintains a live, content-aware index via Spotlight,
//! kept fresh by FSEvents. Rather than build and warm our own whole-disk
//! index, we query that one: instant name *and* content search at ~zero
//! ongoing CPU. This module shells out to `mdfind` (the spike from the
//! plan); a later iteration can swap the implementation for the `MDQuery`
//! C API behind the same signature if we need finer batching control.
//!
//! Per the architecture invariants this crate returns paths only and
//! paints no UI; the GPUI layer turns them into `SearchHit` rows. The
//! caller owns the thread; results stream back through `on_batch` and the
//! walk honors a cooperative `cancel` flag (the child process is killed
//! when it trips).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

/// Where Spotlight should look.
#[derive(Clone, Debug)]
pub enum SpotlightScope {
    /// `-onlyin <dir>`: restrict to a subtree.
    Subtree(PathBuf),
    /// No scope flag: every indexed volume.
    Everywhere,
}

/// Build the `mdfind` argument vector for a query. Pulled out as a pure
/// function so it can be unit-tested without a live index.
///
/// `name_only` uses `-name`, matching the display name only (fast, what
/// most "find a file" intents want). Otherwise the bare query string is
/// Spotlight's natural-language search, which also matches file
/// *content* and metadata.
#[cfg(any(target_os = "macos", test))]
fn mdfind_args(scope: &SpotlightScope, needle: &str, name_only: bool) -> Vec<String> {
    let mut args = Vec::with_capacity(5);
    if let SpotlightScope::Subtree(dir) = scope {
        args.push("-onlyin".to_string());
        args.push(dir.to_string_lossy().into_owned());
    }
    if name_only {
        args.push("-name".to_string());
        args.push(needle.to_string());
    } else {
        args.push(needle.to_string());
    }
    args
}

/// True when Spotlight querying is usable on this system (the `mdfind`
/// binary exists and runs). Callers use it to decide whether to route a
/// global search to Spotlight or fall back to the built-in recursive
/// walker. NOT cheap: `.output()` forks, execs, and waits for the child
///: worker-thread only.
#[cfg(target_os = "macos")]
pub fn spotlight_available() -> bool {
    ferail_core::path_guard::assert_off_ui_thread("spotlight_available");
    std::process::Command::new("mdfind")
        .arg("-help")
        .output()
        .is_ok()
}

#[cfg(not(target_os = "macos"))]
pub fn spotlight_available() -> bool {
    false
}

/// Stream Spotlight results for `needle` within `scope`, in batches of
/// up to `batch_size` paths. `cancel` is checked between lines; when set,
/// the `mdfind` child is killed and the function returns.
///
/// Returns `Err` only when the `mdfind` process could not be spawned
/// (e.g. Spotlight disabled): callers should fall back to the recursive
/// walker in that case.
#[cfg(target_os = "macos")]
pub fn spotlight_search(
    scope: SpotlightScope,
    needle: &str,
    name_only: bool,
    batch_size: usize,
    cancel: &AtomicBool,
    mut on_batch: impl FnMut(Vec<PathBuf>),
) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::atomic::Ordering;

    if needle.trim().is_empty() {
        return Ok(());
    }

    let mut child = Command::new("mdfind")
        .args(mdfind_args(&scope, needle, name_only))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("mdfind produced no stdout"))?;
    let reader = BufReader::new(stdout);

    let mut buffer: Vec<PathBuf> = Vec::with_capacity(batch_size);
    for line in reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            break;
        }
        let Ok(line) = line else { break };
        if line.is_empty() {
            continue;
        }
        buffer.push(PathBuf::from(line));
        if buffer.len() >= batch_size {
            on_batch(std::mem::take(&mut buffer));
            buffer.reserve(batch_size);
        }
    }
    if !buffer.is_empty() {
        on_batch(buffer);
    }
    let _ = child.wait();
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn spotlight_search(
    _scope: SpotlightScope,
    _needle: &str,
    _name_only: bool,
    _batch_size: usize,
    _cancel: &AtomicBool,
    _on_batch: impl FnMut(Vec<PathBuf>),
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Spotlight search is only available on macOS",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_everywhere_natural_language() {
        let args = mdfind_args(&SpotlightScope::Everywhere, "report", false);
        assert_eq!(args, vec!["report"]);
    }

    #[test]
    fn args_subtree_name_only() {
        let args = mdfind_args(
            &SpotlightScope::Subtree(PathBuf::from("/Users/x/Docs")),
            "report",
            true,
        );
        assert_eq!(args, vec!["-onlyin", "/Users/x/Docs", "-name", "report"]);
    }

    #[test]
    fn args_subtree_content() {
        let args = mdfind_args(
            &SpotlightScope::Subtree(PathBuf::from("/tmp")),
            "needle",
            false,
        );
        assert_eq!(args, vec!["-onlyin", "/tmp", "needle"]);
    }

    /// Live-index smoke test. Ignored by default: it depends on the
    /// machine's Spotlight index being enabled and current, which is not
    /// guaranteed in CI / sandboxes. Run with `--ignored` locally.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn finds_a_known_system_path() {
        let cancel = AtomicBool::new(false);
        let mut hits = Vec::new();
        let res = spotlight_search(
            SpotlightScope::Everywhere,
            "Safari",
            true,
            64,
            &cancel,
            |b| hits.extend(b),
        );
        assert!(res.is_ok());
        assert!(hits.iter().any(|p| p.to_string_lossy().contains("Safari")));
    }
}
