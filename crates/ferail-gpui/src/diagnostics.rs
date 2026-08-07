//! Diagnostics — health checks for the app's environment and storage.
//!
//! One `run_checks()` powers two front-ends: the Settings → Diagnostics page
//! (Phase 2) and the `--doctor` CLI flag, so there is a single source of truth
//! and the CLI works even when the GUI won't start.
//!
//! These checks do blocking filesystem I/O (probing writability, stat-ing
//! paths), so per the prime directive **callers must run `run_checks()` off the
//! UI thread** (a background task) and render the cached `Report`. The CLI path
//! runs before the event loop, so it is fine there.
//!
//! The motivating example is the Windows `config_dir()` bug: settings silently
//! never persisted because the config directory resolved to `None`. The
//! "Config directory" check below reports exactly that as a hard `Fail`.

use std::path::{Path, PathBuf};

use crate::app_state;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    /// Fixed-width tag for the plain-text report.
    pub fn tag(self) -> &'static str {
        match self {
            Status::Ok => "OK  ",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        }
    }
}

/// One health check result.
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    /// The filesystem location this check is about, when it has one —
    /// drives the Diagnostics page's "Reveal" jump button. Structured
    /// here (never re-parsed out of the prose `detail`, whose shape
    /// varies per status and gets username-scrubbed in reports).
    /// [`render_text`] ignores it, so text output is unchanged. May
    /// point at a not-yet-created file — reveal then shows the parent.
    pub path: Option<PathBuf>,
}

impl Check {
    fn new(name: &str, status: Status, detail: impl Into<String>) -> Self {
        Check {
            name: name.to_string(),
            status,
            detail: detail.into(),
            path: None,
        }
    }

    fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }
}

/// A titled group of checks (App / Storage / Dependencies / Environment).
pub struct Group {
    pub title: &'static str,
    pub checks: Vec<Check>,
}

/// The full report.
pub struct Report {
    pub app_version: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub groups: Vec<Group>,
}

impl Report {
    /// (#ok, #warn, #fail) across all groups.
    pub fn tally(&self) -> (usize, usize, usize) {
        let mut t = (0, 0, 0);
        for g in &self.groups {
            for c in &g.checks {
                match c.status {
                    Status::Ok => t.0 += 1,
                    Status::Warn => t.1 += 1,
                    Status::Fail => t.2 += 1,
                }
            }
        }
        t
    }

    /// Worst status present, for a one-glance summary badge.
    pub fn worst(&self) -> Status {
        let (_, warn, fail) = self.tally();
        if fail > 0 {
            Status::Fail
        } else if warn > 0 {
            Status::Warn
        } else {
            Status::Ok
        }
    }
}

/// Run every health check. Blocking I/O — call off the UI thread.
pub fn run_checks() -> Report {
    Report {
        app_version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        groups: vec![app_group(), storage_group(), dependencies_group(), environment_group()],
    }
}

// ---- groups -----------------------------------------------------------------

fn app_group() -> Group {
    let build = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let mpv = if cfg!(feature = "mpv") {
        Check::new(
            "mpv support",
            Status::Ok,
            "compiled in (built with --features mpv)",
        )
    } else {
        Check::new(
            "mpv support",
            Status::Warn,
            "not compiled in — rebuild with --features mpv to use the mpv player",
        )
    };
    // The running artifact itself — the `.app` bundle on macOS, the
    // executable elsewhere. Answers "which build am I actually running?"
    // (a stale copy in ~/Downloads vs the one in /Applications).
    let exe = match crate::platform_shell::app_bundle_path() {
        Some(p) => Check::new("Executable", Status::Ok, p.clone()).with_path(p),
        None => Check::new(
            "Executable",
            Status::Warn,
            "could not determine the running executable's path",
        ),
    };
    Group {
        title: "App",
        checks: vec![
            Check::new("Version", Status::Ok, env!("CARGO_PKG_VERSION")),
            Check::new("Build", Status::Ok, build),
            exe,
            mpv,
        ],
    }
}

fn storage_group() -> Group {
    Group {
        title: "Storage",
        checks: vec![check_config_dir(), check_settings_file(), check_metadata_db()],
    }
}

fn dependencies_group() -> Group {
    let state = app_state::load();
    let backend = state.video_backend.as_deref().unwrap_or("builtin");
    let check = if backend == "mpv" {
        let path = state
            .mpv_path
            .clone()
            .unwrap_or_else(|| crate::viewer::backend_native::default_mpv_path().to_string());
        if Path::new(&path).exists() {
            Check::new("mpv install", Status::Ok, format!("{path} (found)")).with_path(&path)
        } else {
            Check::new(
                "mpv install",
                Status::Fail,
                format!("{path} — not found; video will fall back to the built-in player"),
            )
        }
    } else {
        Check::new(
            "Video player",
            Status::Ok,
            "built-in (platform media framework — no external dependency)",
        )
    };
    Group {
        title: "Dependencies",
        checks: vec![check],
    }
}

fn environment_group() -> Group {
    let mut checks = vec![Check::new(
        "Platform",
        Status::Ok,
        format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH),
    )];
    // Presence only — never the values, to keep usernames/paths out of a report.
    for var in env_vars_of_interest() {
        let present = std::env::var_os(var).is_some();
        checks.push(Check::new(
            var,
            if present { Status::Ok } else { Status::Warn },
            if present { "present" } else { "not set" },
        ));
    }
    Group {
        title: "Environment",
        checks,
    }
}

#[cfg(target_os = "windows")]
fn env_vars_of_interest() -> &'static [&'static str] {
    &["APPDATA", "USERPROFILE", "LOCALAPPDATA"]
}

#[cfg(not(target_os = "windows"))]
fn env_vars_of_interest() -> &'static [&'static str] {
    &["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"]
}

// ---- individual storage checks ----------------------------------------------

fn check_config_dir() -> Check {
    match app_state::config_dir() {
        None => Check::new(
            "Config directory",
            Status::Fail,
            "could not resolve a config directory (no APPDATA/HOME) — \
             settings will NOT persist",
        ),
        Some(dir) => match dir_writable(&dir) {
            Ok(()) => Check::new(
                "Config directory",
                Status::Ok,
                format!("{} (writable)", dir.display()),
            )
            .with_path(dir),
            Err(e) => Check::new(
                "Config directory",
                Status::Fail,
                format!("{} — NOT writable: {e}; settings will not persist", dir.display()),
            )
            .with_path(dir),
        },
    }
}

fn check_settings_file() -> Check {
    let Some(path) = app_state::config_path() else {
        return Check::new(
            "Settings file",
            Status::Fail,
            "no path (config directory unresolved)",
        );
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let keys = text.lines().filter(|l| l.contains('=')).count();
            Check::new(
                "Settings file",
                Status::Ok,
                format!("{} ({keys} settings stored)", path.display()),
            )
            .with_path(path)
        }
        Err(_) => Check::new(
            "Settings file",
            Status::Warn,
            format!("{} (not created yet — written on first change)", path.display()),
        )
        .with_path(path),
    }
}

fn check_metadata_db() -> Check {
    let Some(path) = ferail_meta::default_db_path() else {
        return Check::new(
            "Metadata database",
            Status::Warn,
            "no path resolved — running with an in-memory DB (tags/Ant-Trail won't persist)",
        );
    };
    let parent_ok = path.parent().map(dir_writable).transpose();
    match (path.exists(), parent_ok) {
        (true, _) => Check::new(
            "Metadata database",
            Status::Ok,
            format!("{} (present)", path.display()),
        )
        .with_path(path),
        (false, Ok(_)) => Check::new(
            "Metadata database",
            Status::Warn,
            format!("{} (not created yet — written on first use)", path.display()),
        )
        .with_path(path),
        (false, Err(e)) => Check::new(
            "Metadata database",
            Status::Fail,
            format!("{} — directory not writable: {e}", path.display()),
        )
        .with_path(path),
    }
}

/// Probe whether `dir` is writable by creating it (if needed) and round-
/// tripping a tiny file. Cleans up after itself.
fn dir_writable(dir: &Path) -> Result<(), String> {
    if !dir.exists() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let probe = dir.join(".ferail-write-probe");
    std::fs::write(&probe, b"ok").map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

// ---- rendering --------------------------------------------------------------

/// Render the report as plain text — for `--doctor`, the "Copy report" button,
/// and the bundled report in an issue report.
pub fn render_text(report: &Report) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let build = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let _ = writeln!(
        s,
        "Ferail diagnostics — v{} ({build}, {}/{})",
        report.app_version, report.os, report.arch
    );
    for group in &report.groups {
        let _ = writeln!(s, "\n[{}]", group.title);
        for c in &group.checks {
            let _ = writeln!(s, "  {}  {:<18} {}", c.status.tag(), c.name, c.detail);
        }
    }
    let (ok, warn, fail) = report.tally();
    let _ = writeln!(s, "\n{ok} OK, {warn} WARN, {fail} FAIL");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_runs_and_renders() {
        let report = run_checks();
        // App group always has Version/Build/mpv; total groups are fixed.
        assert_eq!(report.groups.len(), 4);
        assert!(report.groups.iter().any(|g| g.title == "Storage"));
        let text = render_text(&report);
        assert!(text.contains("Ferail diagnostics"));
        assert!(text.contains("[Storage]"));
        assert!(text.contains("Config directory"));
        assert!(text.contains("Executable"));
        // The jump targets: the running-executable row and the storage
        // rows carry a structured path for the Reveal button.
        let app = report.groups.iter().find(|g| g.title == "App").unwrap();
        let exe = app.checks.iter().find(|c| c.name == "Executable").unwrap();
        assert!(exe.path.is_some(), "executable path resolves in tests");
        let storage = report.groups.iter().find(|g| g.title == "Storage").unwrap();
        assert!(storage.checks.iter().all(|c| c.path.is_some()));
        // tally + worst are internally consistent.
        let (ok, warn, fail) = report.tally();
        assert!(ok + warn + fail >= 7);
    }
}
