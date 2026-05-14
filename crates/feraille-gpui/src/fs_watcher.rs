//! Thin wrapper around `notify` that bridges OS file-system events
//! to GPUI's foreground executor.
//!
//! `notify` runs the watcher on its own platform thread and calls
//! our handler closure from there — we cannot mutate GPUI state
//! directly from that thread. Instead the handler pushes events
//! onto an `std::sync::mpsc` channel, and a foreground-executor
//! task polls the receiver on a short timer. When the poll sees a
//! pending event it asks the Shell to reload, which runs on the
//! UI thread (the only place that can mutate `Entity<TableState>`).
//!
//! Poll interval: 250 ms. Short enough that external changes feel
//! immediate, long enough that idle CPU stays near zero. A more
//! sophisticated future implementation would dispatch via
//! `cx.background_executor().spawn()` and an async channel, but
//! the polling shape works on all GPUI versions and is easy to
//! reason about.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use anyhow::Result;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Owns the underlying `notify::Watcher` plus the receiver end of
/// the channel. Dropping it stops the watch.
pub struct FsWatcher {
    watcher: RecommendedWatcher,
    receiver: Receiver<Event>,
    watched: HashSet<PathBuf>,
}

impl FsWatcher {
    pub fn new() -> Result<Self> {
        let (tx, rx) = channel();
        let watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(ev) = res {
                // Drop send errors silently — the receiver side may
                // have been torn down (window closed) before the
                // watcher thread finished its last batch.
                let _ = tx.send(ev);
            }
        })?;
        Ok(Self {
            watcher,
            receiver: rx,
            watched: HashSet::new(),
        })
    }

    /// Add `path` to the watched directory set. Non-recursive: each
    /// visible tab/window directory registers itself explicitly, and
    /// reload fan-out decides which views need refreshing.
    pub fn watch(&mut self, path: &Path) -> Result<()> {
        if self.watched.contains(path) {
            return Ok(());
        }
        self.watcher.watch(path, RecursiveMode::NonRecursive)?;
        self.watched.insert(path.to_path_buf());
        Ok(())
    }

    /// Drain any pending events and return the watched directory roots
    /// affected by relevant mutations. Filters out access-only events
    /// (Open / ReadAccess) since they don't change the listing.
    pub fn drain_reload_relevant_paths(&self) -> Vec<PathBuf> {
        let mut relevant = HashSet::new();
        while let Ok(ev) = self.receiver.try_recv() {
            let should_reload = matches!(
                ev.kind,
                EventKind::Create(_)
                    | EventKind::Modify(_)
                    | EventKind::Remove(_)
                    | EventKind::Other
            );
            if !should_reload {
                continue;
            }

            if ev.paths.is_empty() {
                relevant.extend(self.watched.iter().cloned());
                continue;
            }

            for changed in &ev.paths {
                for root in &self.watched {
                    if changed == root || changed.parent() == Some(root.as_path()) {
                        relevant.insert(root.clone());
                    }
                }
            }
        }
        relevant.into_iter().collect()
    }
}

/// Recommended poll interval for the foreground-executor polling
/// task. 250 ms gives near-immediate response without spinning.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);
