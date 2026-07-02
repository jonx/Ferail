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
            match res {
                Ok(ev) => {
                    // Drop send errors silently — the receiver side may
                    // have been torn down (window closed) before the
                    // watcher thread finished its last batch.
                    let _ = tx.send(ev);
                }
                Err(_) => {
                    // A watcher *error* is notify's "events were lost,
                    // rescan" signal (inotify queue overflow, FSEvents
                    // must-rescan). Dropping it would leave listings
                    // permanently stale — surface it as an empty-paths
                    // event, which the drain treats as everything-dirty.
                    let _ = tx.send(Event::new(EventKind::Other));
                }
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

    /// Retain only the directories some live tab still shows, releasing
    /// every other OS watch. Without this the watch set was add-only —
    /// every directory ever visited stayed FSEvents/inotify-watched for
    /// the whole session (Linux walks into `max_user_watches`, after
    /// which *new* watches silently fail and live-update stops).
    /// Callers pass the union of every window's tab directories after
    /// navigation/close events.
    pub fn retain_watched(&mut self, keep: &HashSet<PathBuf>) {
        let stale: Vec<PathBuf> = self
            .watched
            .iter()
            .filter(|p| !keep.contains(*p))
            .cloned()
            .collect();
        for path in stale {
            let _ = self.watcher.unwatch(&path);
            self.watched.remove(&path);
        }
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

/// Minimum interval between reloads of the *same* directory. The poll
/// already coalesces all events within one 250 ms window into a single
/// reload; this throttles *across* windows, so a burst — a multi-file
/// paste, a download landing, an editor's create+rename+modify save —
/// collapses into far fewer re-enumerate + prefetch passes instead of one
/// per window. Leading-edge: the first event reloads immediately; only
/// repeats inside the window are deferred, and a still-changing directory
/// keeps reloading once per window — so a real change is never dropped,
/// only rate-limited. This is throttling, not suppression: it does not
/// compare contents or skip work that would surface a change.
pub const RELOAD_DEBOUNCE: Duration = Duration::from_millis(750);
