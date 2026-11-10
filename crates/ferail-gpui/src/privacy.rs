//! What Ferail is allowed to remember (docs/features/PRIVACY_SETTINGS.md).
//!
//! One [`Policy`], read from [`crate::app_state`], decides four things:
//! whether the metadata store writes to disk, which locations are never
//! recorded, whether the session (last folder and open tabs) is saved, and
//! whether thumbnails may come from the operating system's thumbnail
//! service. Everything here is either pure or runs on the background
//! executor: applying a policy opens no file on the UI thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use ferail_core::privacy::Exclusions;
use gpui::{App, AppContext as _};

use crate::app_state::{self, AppState};

/// The privacy settings, with defaults resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Master switch: nothing about what is browsed reaches the disk.
    pub remember_nothing: bool,
    /// Reopen the last session's tabs at launch.
    pub restore_session: bool,
    /// Record visited folders (Recents and the Ant Trail feed on them).
    pub record_visits: bool,
    /// Locations nothing is recorded under.
    pub exclusions: Exclusions,
    /// Days an unrefreshed visit or cache row survives.
    pub retention_days: u32,
    /// Days a report file is kept; `None` keeps them until deleted by hand.
    pub report_retention_days: Option<u32>,
    /// Whether the system thumbnail service may be used.
    pub system_thumbnails: bool,
}

impl Policy {
    pub fn from_state(state: &AppState) -> Self {
        let remember_nothing = state.remember_nothing.unwrap_or(false);
        Self {
            remember_nothing,
            restore_session: state.restore_session.unwrap_or(true),
            record_visits: state.record_visits.unwrap_or(true),
            exclusions: Exclusions::new(
                state.privacy_excluded.iter().cloned(),
                state.privacy_exclude_volumes.unwrap_or(false),
            ),
            retention_days: state
                .retention_days
                .unwrap_or(app_state::DEFAULT_RETENTION_DAYS),
            report_retention_days: state.report_retention_days.filter(|days| *days > 0),
            // The system service keeps its own cache of everything it
            // renders, so "remember nothing" has to bypass it too.
            system_thumbnails: state.system_thumbnails.unwrap_or(true) && !remember_nothing,
        }
    }

    /// The policy in force. Cache-backed, no I/O after the first load.
    pub fn current() -> Self {
        Self::from_state(&app_state::load())
    }

    /// Whether the last folder and the open tabs are saved and restored.
    pub fn keeps_session(&self) -> bool {
        self.restore_session && !self.remember_nothing
    }

    /// The session to save for a window showing `tabs` (folder tabs only,
    /// in order) with `active` in front: the last folder, the tab list and
    /// the index of the front tab within it. Tabs in excluded locations
    /// are dropped, and nothing at all is saved when the session is not
    /// kept.
    pub fn session(&self, tabs: &[PathBuf], active: Option<usize>) -> Session {
        if !self.keeps_session() {
            return Session::default();
        }
        let mut kept = Vec::new();
        let mut front = None;
        for (index, tab) in tabs.iter().enumerate() {
            if self.exclusions.covers(tab) {
                continue;
            }
            if Some(index) == active {
                front = Some(kept.len());
            }
            kept.push(tab.clone());
        }
        kept.truncate(app_state::MAX_SESSION_TABS);
        let front = front.filter(|index| *index < kept.len());
        Session {
            last_dir: front.or(kept.len().checked_sub(1)).map(|i| kept[i].clone()),
            active: front,
            tabs: kept,
        }
    }
}

/// What [`Policy::session`] lets through to the state file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    pub last_dir: Option<PathBuf>,
    pub tabs: Vec<PathBuf>,
    pub active: Option<usize>,
}

impl Session {
    /// Write this session into `state`, replacing the previous one.
    pub fn store(self, state: &mut AppState) {
        state.last_dir = self.last_dir;
        state.session_tabs = self.tabs;
        state.session_active = self.active;
    }
}

/// Mirror of [`Policy::system_thumbnails`] for the thumbnail workers, which
/// run on the background pool and ask once per file.
static SYSTEM_THUMBNAILS: AtomicBool = AtomicBool::new(true);

/// Whether a thumbnail or preview may be fetched from the operating
/// system's thumbnail service. Off, callers fall through to Ferail's own
/// decoders.
pub fn system_thumbnails_allowed() -> bool {
    SYSTEM_THUMBNAILS.load(Ordering::Relaxed)
}

/// Set for a run that must not touch the user's saved session: the
/// screenshot harness opens windows on fixture folders.
static SESSION_FROZEN: AtomicBool = AtomicBool::new(false);

/// Leave the saved session as it is for the rest of this process.
pub fn freeze_session() {
    SESSION_FROZEN.store(true, Ordering::Relaxed);
}

pub fn session_frozen() -> bool {
    SESSION_FROZEN.load(Ordering::Relaxed)
}

/// Bring the process in line with the stored settings: the thumbnail
/// switch at once, the metadata store on the background executor. Called
/// at window boot and after every privacy setting change.
pub fn apply(cx: &mut App) {
    let policy = Policy::current();
    SYSTEM_THUMBNAILS.store(policy.system_thumbnails, Ordering::Relaxed);
    if !policy.keeps_session() && !session_frozen() {
        let mut state = app_state::load();
        if state.last_dir.is_some() || !state.session_tabs.is_empty() {
            Session::default().store(&mut state);
            app_state::save(&state);
        }
    }
    let Some(db) = process(cx).and_then(|process| process.db_snapshot()) else {
        return;
    };
    cx.background_spawn(async move {
        if let Ok(mut guard) = db.lock() {
            apply_to_db(&mut guard, &policy);
        }
    })
    .detach();
}

/// The process singleton, absent when Settings is the only window ever
/// opened (`--settings`): then there is no store or window to update.
fn process(cx: &App) -> Option<std::rc::Rc<crate::process_state::ProcessState>> {
    cx.try_global::<crate::process_state::ProcessStateGlobal>()
        .map(|global| global.0.clone())
}

/// Configure a metadata store for `policy`. Blocking (SQLite): background
/// executor only.
pub fn apply_to_db(db: &mut ferail_meta::MetadataDb, policy: &Policy) {
    if let Err(error) = db.set_ephemeral(policy.remember_nothing) {
        crate::log_warn!(90, "privacy: session-only store failed: {error}");
    }
    db.set_exclusions(policy.exclusions.clone());
    if let Err(error) = db.purge_excluded() {
        crate::log_warn!(90, "privacy: purge of excluded locations failed: {error}");
    }
}

/// One thing the Privacy page can erase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClearTarget {
    /// Visited folders: Recents and Ant Trail heat.
    Visits,
    /// What Ferail derived about listed files and folders.
    FileCaches,
    /// Where trashed items came from.
    PutBack,
    /// The saved last folder and tab list.
    Session,
    /// Crash, freeze and issue-report files.
    Reports,
}

/// Erase `target`, in memory now and on disk on the background executor.
pub fn clear(target: ClearTarget, cx: &mut App) {
    let process = process(cx);
    let scope = match target {
        ClearTarget::Visits => {
            if let Some(process) = process.as_ref() {
                process.ant_visits.borrow_mut().clear();
                process.ant_max.set(0);
                process.recents.borrow_mut().clear();
                for shell in process.live_shells() {
                    let _ = shell.update(cx, |shell, cx| shell.visits_cleared(cx));
                }
            }
            Some(ferail_meta::ResetScope::AntTrail)
        }
        ClearTarget::FileCaches => Some(ferail_meta::ResetScope::FileCaches),
        ClearTarget::PutBack => Some(ferail_meta::ResetScope::PutBack),
        ClearTarget::Session => {
            let mut state = app_state::load();
            Session::default().store(&mut state);
            app_state::save(&state);
            None
        }
        ClearTarget::Reports => {
            cx.background_spawn(async move { remove_reports(None) })
                .detach();
            None
        }
    };
    let Some(scope) = scope else {
        return;
    };
    let shared = process.and_then(|process| process.db_snapshot());
    cx.background_spawn(async move {
        match shared {
            Some(db) => {
                if let Ok(guard) = db.lock() {
                    reset_and_compact(&guard, scope);
                }
            }
            // No window has opened the store (Settings launched on its
            // own, or safe mode): the data is on disk all the same.
            None => {
                let opened = ferail_meta::default_db_path()
                    .filter(|path| path.exists())
                    .map(|path| ferail_meta::MetadataDb::open(&path));
                match opened {
                    Some(Ok(db)) => reset_and_compact(&db, scope),
                    Some(Err(error)) => crate::log_warn!(90, "privacy: open failed: {error}"),
                    None => {}
                }
            }
        }
    })
    .detach();
}

fn reset_and_compact(db: &ferail_meta::MetadataDb, scope: ferail_meta::ResetScope) {
    if let Err(error) = db.reset(scope) {
        crate::log_warn!(90, "privacy: clear failed: {error}");
    }
    // Deleted rows are zeroed in place; this hands the pages back and
    // empties the write-ahead log that still holds copies.
    if let Err(error) = db.compact() {
        crate::log_warn!(90, "privacy: compact failed: {error}");
    }
}

/// Delete report files older than the retention setting. Blocking:
/// background executor only.
pub fn prune_reports(policy: &Policy) {
    if let Some(days) = policy.report_retention_days {
        remove_reports(Some(std::time::Duration::from_secs(
            u64::from(days) * 86_400,
        )));
    }
}

/// Delete Ferail's report files, all of them or those older than
/// `older_than`. Blocking: background executor only.
fn remove_reports(older_than: Option<std::time::Duration>) {
    let Some(dir) = app_state::config_dir().map(|dir| dir.join("reports")) else {
        return;
    };
    remove_reports_in(&dir, older_than, std::time::SystemTime::now());
}

fn remove_reports_in(
    dir: &Path,
    older_than: Option<std::time::Duration>,
    now: std::time::SystemTime,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // Only files Ferail wrote: the folder is the user's to keep other
        // things in.
        if !entry.file_name().to_string_lossy().starts_with("ferail-") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let expired = match older_than {
            None => true,
            Some(limit) => meta
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > limit),
        };
        if expired {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(state: AppState) -> Policy {
        Policy::from_state(&state)
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn defaults_remember_and_restore() {
        let fresh = policy(AppState::default());
        assert!(!fresh.remember_nothing);
        assert!(fresh.restore_session);
        assert!(fresh.record_visits);
        assert!(fresh.system_thumbnails);
        assert!(fresh.exclusions.is_empty());
        assert_eq!(fresh.retention_days, app_state::DEFAULT_RETENTION_DAYS);
        assert_eq!(fresh.report_retention_days, None);
    }

    #[test]
    fn remember_nothing_also_bypasses_the_system_thumbnail_service() {
        let private = policy(AppState {
            remember_nothing: Some(true),
            system_thumbnails: Some(true),
            ..AppState::default()
        });
        assert!(!private.system_thumbnails);
        assert!(!private.keeps_session());
    }

    #[test]
    fn session_keeps_tab_order_and_the_front_tab() {
        let saved = policy(AppState::default()).session(&paths(&["/a", "/b", "/c"]), Some(1));
        assert_eq!(saved.tabs, paths(&["/a", "/b", "/c"]));
        assert_eq!(saved.active, Some(1));
        assert_eq!(saved.last_dir, Some(PathBuf::from("/b")));
    }

    #[test]
    fn session_is_empty_when_not_kept() {
        for state in [
            AppState {
                restore_session: Some(false),
                ..AppState::default()
            },
            AppState {
                remember_nothing: Some(true),
                ..AppState::default()
            },
        ] {
            let saved = policy(state).session(&paths(&["/a", "/b"]), Some(0));
            assert_eq!(saved, Session::default());
        }
    }

    #[test]
    fn session_drops_tabs_in_excluded_locations() {
        let private = policy(AppState {
            privacy_excluded: paths(&["/secret"]),
            ..AppState::default()
        });
        let saved = private.session(&paths(&["/a", "/secret/x", "/b"]), Some(2));
        assert_eq!(saved.tabs, paths(&["/a", "/b"]));
        assert_eq!(saved.active, Some(1));

        // The front tab itself is excluded: nothing points at it.
        let saved = private.session(&paths(&["/a", "/secret/x"]), Some(1));
        assert_eq!(saved.tabs, paths(&["/a"]));
        assert_eq!(saved.active, None);
        assert_eq!(saved.last_dir, Some(PathBuf::from("/a")));

        let saved = private.session(&paths(&["/secret"]), Some(0));
        assert_eq!(saved, Session::default());
    }

    #[test]
    fn report_pruning_only_touches_old_ferail_files() {
        let dir = std::env::temp_dir().join(format!("ferail-privacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ferail-crash-1.txt"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"mine").unwrap();
        let day = std::time::Duration::from_secs(86_400);
        let now = std::time::SystemTime::now();

        remove_reports_in(&dir, Some(7 * day), now);
        assert!(dir.join("ferail-crash-1.txt").exists(), "still fresh");

        remove_reports_in(&dir, Some(7 * day), now + 8 * day);
        assert!(!dir.join("ferail-crash-1.txt").exists());
        assert!(dir.join("notes.txt").exists());

        std::fs::write(dir.join("ferail-hang-2-0.txt"), b"x").unwrap();
        remove_reports_in(&dir, None, now);
        assert!(!dir.join("ferail-hang-2-0.txt").exists());
        assert!(dir.join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
