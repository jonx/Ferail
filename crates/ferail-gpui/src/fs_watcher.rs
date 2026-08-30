//! Thin wrapper around `notify` that bridges OS file-system events
//! to GPUI's foreground executor, and keeps every blocking watcher
//! call off the UI thread.
//!
//! Two thread boundaries live here:
//!
//! 1. **Events in.** `notify` runs the watcher on its own platform
//!    thread and calls our handler closure from there: we cannot
//!    mutate GPUI state directly from that thread. The handler pushes
//!    events onto an `std::sync::mpsc` channel, and a
//!    foreground-executor task polls the receiver on a short timer.
//!    When the poll sees a pending event it asks the Shell to reload,
//!    which runs on the UI thread (the only place that can mutate
//!    `Entity<TableState>`).
//!
//! 2. **Commands out.** Registering a watch is NOT cheap:
//!    `notify`'s FSEvents backend calls `path.canonicalize()` and
//!    `path.exists()` inside `Watcher::watch()`: real filesystem
//!    syscalls that block until a spun-down external drive or cold
//!    network mount answers (seconds). Calling that from a navigation
//!    handler froze the whole UI before enumeration even started:
//!    a Prime Directive violation. So the `notify::Watcher` object
//!    lives on a dedicated `fs-watcher` worker thread; the UI-side
//!    [`FsWatcher`] handle just sends [`WatchCmd`]s over a channel
//!    and returns immediately.
//!
//! Poll interval: 250 ms. Short enough that external changes feel
//! immediate, long enough that idle CPU stays near zero.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use anyhow::Result;
use notify::{Event, EventKind, RecursiveMode, Watcher};

/// Watch-set mutations shipped to the worker thread. Everything that
/// touches the underlying `notify::Watcher` goes through one of these.
enum WatchCmd {
    /// Register a non-recursive watch on a directory (idempotent).
    Watch(PathBuf),
    /// Drop every watch not in the given keep-set.
    Retain(HashSet<PathBuf>),
}

/// UI-side handle: the event receiver plus the command channel into
/// the worker thread that owns the real `notify::Watcher`. Dropping it
/// closes the command channel, which ends the worker and stops the
/// watch.
pub struct FsWatcher {
    commands: Sender<WatchCmd>,
    receiver: Receiver<Event>,
    /// UI-side mirror of the *requested* watch roots. Used only to
    /// match incoming event paths to roots in
    /// [`Self::drain_reload_relevant_paths`]; the worker keeps the
    /// authoritative set (a requested watch may have failed, then no
    /// events arrive for it and the mirror entry is inert).
    watched: HashSet<PathBuf>,
}

impl FsWatcher {
    pub fn new() -> Result<Self> {
        let (tx, rx) = channel();
        // Constructing the watcher itself does no path I/O: it just
        // spins up the FSEvents/inotify machinery, so it's safe here;
        // only `watch`/`unwatch` calls must stay on the worker.
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            match res {
                Ok(ev) => {
                    // Drop send errors silently: the receiver side may
                    // have been torn down (window closed) before the
                    // watcher thread finished its last batch.
                    let _ = tx.send(ev);
                }
                Err(_) => {
                    // A watcher *error* is notify's "events were lost,
                    // rescan" signal (inotify queue overflow, FSEvents
                    // must-rescan). Dropping it would leave listings
                    // permanently stale: surface it as an empty-paths
                    // event, which the drain treats as everything-dirty.
                    let _ = tx.send(Event::new(EventKind::Other));
                }
            }
        })?;

        let (cmd_tx, cmd_rx) = channel::<WatchCmd>();
        std::thread::Builder::new()
            .name("fs-watcher".into())
            .spawn(move || {
                // Authoritative watch set: only successful registrations
                // enter it, so a failed watch (unmounted volume, vanished
                // dir) is retried the next time a navigation requests it.
                let mut watched: HashSet<PathBuf> = HashSet::new();
                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        WatchCmd::Watch(path) => {
                            if watched.contains(&path) {
                                continue;
                            }
                            // This worker thread is the one sanctioned
                            // home of the raw notify calls the lint bans.
                            #[allow(clippy::disallowed_methods)]
                            if watcher.watch(&path, RecursiveMode::NonRecursive).is_ok() {
                                watched.insert(path);
                            }
                        }
                        WatchCmd::Retain(keep) => {
                            let stale: Vec<PathBuf> = watched
                                .iter()
                                .filter(|p| !keep.contains(*p))
                                .cloned()
                                .collect();
                            for path in stale {
                                // Sanctioned: worker thread (see above).
                                #[allow(clippy::disallowed_methods)]
                                let _ = watcher.unwatch(&path);
                                watched.remove(&path);
                            }
                        }
                    }
                }
                // Command sender dropped (FsWatcher gone): fall out of
                // the loop, dropping the watcher and every OS watch.
            })?;

        Ok(Self {
            commands: cmd_tx,
            receiver: rx,
            watched: HashSet::new(),
        })
    }

    /// Request a watch on `path`. Non-recursive: each visible
    /// tab/window directory registers itself explicitly, and reload
    /// fan-out decides which views need refreshing. Returns
    /// immediately: the actual (possibly blocking) OS registration
    /// happens on the worker thread; failures there are non-fatal (the
    /// user just loses live updates for that directory) and retried on
    /// the next request.
    pub fn watch(&mut self, path: &Path) {
        self.watched.insert(path.to_path_buf());
        let _ = self.commands.send(WatchCmd::Watch(path.to_path_buf()));
    }

    /// Retain only the directories some live tab still shows, releasing
    /// every other OS watch. Without this the watch set was add-only:
    /// every directory ever visited stayed FSEvents/inotify-watched for
    /// the whole session (Linux walks into `max_user_watches`, after
    /// which *new* watches silently fail and live-update stops).
    /// Callers pass the union of every window's tab directories after
    /// navigation/close events. The unwatch syscalls run on the worker.
    pub fn retain_watched(&mut self, keep: &HashSet<PathBuf>) {
        self.watched.retain(|p| keep.contains(p));
        let _ = self.commands.send(WatchCmd::Retain(keep.clone()));
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
/// reload; this throttles *across* windows, so a burst: a multi-file
/// paste, a download landing, an editor's create+rename+modify save:
/// collapses into far fewer re-enumerate + prefetch passes instead of one
/// per window. Leading-edge: the first event reloads immediately; only
/// repeats inside the window are deferred, and a still-changing directory
/// keeps reloading once per window, so a real change is never dropped,
/// only rate-limited. This is throttling, not suppression: it does not
/// compare contents or skip work that would surface a change.
pub const RELOAD_DEBOUNCE: Duration = Duration::from_millis(750);
