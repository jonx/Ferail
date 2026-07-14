//! Linux platform shell.
//!
//! This crate is the Linux arm of the `platform_shell` indirection (see
//! [`feraille_gpui::platform_shell`] and `docs/features/linux-port.md`). Its
//! job is to mirror the public `pub fn` surface that `feraille-gpui` reaches
//! through `crate::platform_shell::*`, so that the app compiles, links, and —
//! increasingly — *works* on `target_os = "linux"`.
//!
//! ## Pattern
//!
//! Every function has a real implementation behind `#[cfg(target_os = "linux")]`
//! and a no-op twin behind `#[cfg(not(target_os = "linux"))]`. The twin exists
//! purely so the crate still compiles on macOS/Windows as a workspace member —
//! it is never reached through the alias, because cargo only links the matching
//! shell crate per target. Shared types are declared unconditionally with
//! identical shape across all three shell crates so they round-trip through the
//! alias.
//!
//! The real arms can be **type-checked from any host** with
//! `cargo check --target x86_64-unknown-linux-gnu -p feraille-shell-linux`
//! (`cargo check` doesn't link, so no Linux system libraries are required for
//! the pure-`std` / process-based functions implemented so far).
//!
//! ## Surface contract & status
//!
//! The signatures below are the exact subset of `feraille-shell-mac` /
//! `feraille-shell-win32` that gpui invokes through the alias. Callback bounds
//! match **macOS** (`Box<dyn Fn(..) + 'static>`, no `Send`) — the proven-green
//! contract. `docs/features/linux-port.md` §6 maps every still-stubbed function
//! to the freedesktop / D-Bus / XDG mechanism to reach for; run all blocking
//! work off the UI thread.
//!
//! Implemented: `duplicate_path`, `make_alias`, `make_alias_in`, `open_url`,
//! `reveal_in_finder`, `open_terminal`, `system_is_dark`, `open_with_app`,
//! `open_with_candidates` (freedesktop MIME + `.desktop` scan), and
//! `copy_to_clipboard` (plain text via `arboard`). Still stubbed: the file-URL
//! clipboard, thumbnails, the dark/volume/power observers, and video — these
//! need richer `ashpd`/`zbus`/`gstreamer` integration (linux-port.md §6).
//! (Trash and download-provenance live in `feraille-fs-native`, not here.)

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use feraille_core::commands::TagColor;
use feraille_core::power::PowerEvent;

// =============================================================
// Shared types (declared with identical shape in every shell crate
// so they round-trip through the `platform_shell` alias).
// =============================================================

/// An application Launch-Services-style "Open With" would offer for a path.
/// On Linux this will come from MIME associations (`mimeapps.list` +
/// `.desktop` entries); see linux-port.md §6.
#[derive(Debug, Clone)]
pub struct OpenWithCandidate {
    pub name: String,
    pub path: PathBuf,
    pub is_default: bool,
}

/// Result of [`set_app_icon_from_png_bytes`], mirrored from shell-mac /
/// shell-win32. `NotMacOs` is retained as a variant name purely so the `Debug`
/// output matches the other shells one-for-one. On Linux the runtime icon swap
/// is a no-op (icon identity comes from the `.desktop` file + Wayland
/// `app_id` / X11 `WM_CLASS`), so this returns `NotMacOs`.
#[derive(Debug)]
pub enum SetIconResult {
    Ok,
    NotMacOs,
    NotMainThread,
    DecodeFailed,
}

/// RAII guard that keeps the system awake while held — twin of shell-mac's
/// `SleepBlocker`. On Linux it owns a `systemd-inhibit` child holding an `idle`
/// inhibitor lock; dropping the guard kills the child, releasing the lock. The
/// shape differs per platform (it carries a `Child` only on Linux), which is
/// fine because the type is only ever constructed/used on its own target.
#[cfg(target_os = "linux")]
pub struct SleepBlocker {
    child: std::process::Child,
}
#[cfg(target_os = "linux")]
impl Drop for SleepBlocker {
    fn drop(&mut self) {
        // Kill the systemd-inhibit child (releasing its lock) and reap it so we
        // don't leave a zombie. Errors here are benign (already exited).
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
#[cfg(not(target_os = "linux"))]
pub struct SleepBlocker;

// =============================================================
// App lifecycle / chrome / about
// =============================================================

/// Configure the About-panel text. No global menu bar on Linux (the title-bar
/// hamburger covers about/settings) — no-op for v1.
pub fn set_about_options(_app_name: &str, _tagline: &str, _version: &str, _copyright: &str) {}

/// Show the About panel. No-op until an in-window about surface exists.
pub fn show_about_panel() {}

/// Whether a "Show Desktop" affordance is available. Linux has no portable
/// minimize-all primitive across compositors — `false` hides the menu item.
pub fn show_desktop_available() -> bool {
    false
}

/// Minimize-all / show desktop. Returns whether it acted. No portable Linux
/// equivalent yet — `false`.
pub fn show_desktop() -> bool {
    false
}

/// Raise every app window preserving z-order (macOS `arrangeInFront:`).
/// No portable Linux/AROS primitive — `false` makes the caller fall
/// back to raising each window through gpui.
pub fn bring_all_windows_to_front() -> bool {
    false
}

// =============================================================
// Pickers / launching
// =============================================================

/// Pick a folder. Real impl: portal `org.freedesktop.portal.FileChooser` via
/// `ashpd`. `None` = cancelled / unavailable.
pub fn pick_folder() -> Option<PathBuf> {
    None
}

/// Open a URL (or `file://` path) in the user's default handler via
/// `xdg-open`, spawned detached so the caller never blocks on the child.
#[cfg(target_os = "linux")]
pub fn open_url(url: &str) {
    let _ = spawn_detached(std::process::Command::new("xdg-open").arg(url));
}
#[cfg(not(target_os = "linux"))]
pub fn open_url(_url: &str) {}

/// Reveal a path in the user's file manager. Tries the cross-desktop D-Bus
/// `org.freedesktop.FileManager1.ShowItems` (Nautilus/Dolphin/Nemo/Files all
/// implement it, and it highlights the item in its parent), falling back to
/// opening the parent directory with `xdg-open`.
///
/// Blocks for the D-Bus method REPLY (`--print-reply`) — without it,
/// `dbus-send` exits 0 the moment the message is *sent*, so on
/// desktops with no FileManager1 implementation (i3/sway/minimal) the
/// "success" was a lie and the xdg-open fallback was dead code.
/// Because it waits (session-bus round-trip; cold D-Bus activation of
/// the file manager can take seconds), callers must run this on a
/// worker, never the UI thread.
#[cfg(target_os = "linux")]
pub fn reveal_in_finder(path: &Path) {
    let uri = file_uri(path);
    let shown = std::process::Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.FileManager1",
            "--type=method_call",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
        ])
        .arg(format!("array:string:{uri}"))
        .arg("string:")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !shown {
        let parent = path.parent().unwrap_or(path);
        let _ = spawn_detached(std::process::Command::new("xdg-open").arg(parent));
    }
}
#[cfg(not(target_os = "linux"))]
pub fn reveal_in_finder(_path: &Path) {}

/// Open a terminal emulator with its working directory set to `path`.
/// Detection chain: `$TERMINAL`, then the Debian `x-terminal-emulator`
/// alternative, then a probe list of common emulators. The child inherits
/// `current_dir(path)`, which is how virtually every emulator picks its
/// initial shell directory.
#[cfg(target_os = "linux")]
pub fn open_terminal(path: &Path) {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(t) = std::env::var("TERMINAL") {
        if !t.is_empty() {
            candidates.push(t);
        }
    }
    candidates.extend(
        [
            "x-terminal-emulator",
            "gnome-terminal",
            "kgx", // GNOME Console
            "konsole",
            "kitty",
            "alacritty",
            "wezterm",
            "xfce4-terminal",
            "tilix",
            "foot",
            "xterm",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    for term in candidates {
        let mut cmd = std::process::Command::new(&term);
        cmd.current_dir(path);
        if spawn_detached(&mut cmd).is_ok() {
            return;
        }
    }
}
#[cfg(not(target_os = "linux"))]
pub fn open_terminal(_path: &Path) {}

/// Enumerate the applications that can open `path`, freedesktop-style: resolve
/// the file's MIME type, find the registered default, then scan the XDG
/// application directories for `.desktop` entries that declare that MIME in
/// their `MimeType=` list. The candidate `path` is the `.desktop` file, which
/// [`open_with_app`] launches via `gio launch`. This is the Linux analogue of
/// macOS LaunchServices / Windows `SHAssocEnumHandlers`.
///
/// MIME resolution and the default handler shell out to `xdg-mime`; the scan
/// itself is pure `std` (see [`candidates_for_mime`], which is unit-tested).
#[cfg(target_os = "linux")]
pub fn open_with_candidates(path: &Path) -> Vec<OpenWithCandidate> {
    let mime = mime_type_for(path);
    if mime.is_empty() {
        return Vec::new();
    }
    let default_id = xdg_default_desktop(&mime);
    candidates_for_mime(&mime, &default_id)
}
#[cfg(not(target_os = "linux"))]
pub fn open_with_candidates(_path: &Path) -> Vec<OpenWithCandidate> {
    Vec::new()
}

/// The file's MIME type via `xdg-mime query filetype` (empty if unavailable).
#[cfg(target_os = "linux")]
fn mime_type_for(path: &Path) -> String {
    std::process::Command::new("xdg-mime")
        .arg("query")
        .arg("filetype")
        .arg(path)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// The default handler's `.desktop` id for `mime` via `xdg-mime query default`.
#[cfg(target_os = "linux")]
fn xdg_default_desktop(mime: &str) -> String {
    std::process::Command::new("xdg-mime")
        .arg("query")
        .arg("default")
        .arg(mime)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// XDG application directories in precedence order: `$XDG_DATA_HOME/applications`
/// (or `~/.local/share/applications`) first, then each `$XDG_DATA_DIRS` entry
/// (default `/usr/local/share:/usr/share`).
#[cfg(target_os = "linux")]
fn xdg_application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    if let Some(dh) = data_home {
        dirs.push(dh.join("applications"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    for d in data_dirs.split(':').filter(|s| !s.is_empty()) {
        dirs.push(PathBuf::from(d).join("applications"));
    }
    dirs
}

/// The `[Desktop Entry]` fields we need to decide whether an entry is an
/// offerable handler.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct DesktopEntry {
    entry_type: Option<String>,
    name: Option<String>,
    mimetypes: Vec<String>,
    no_display: bool,
    hidden: bool,
}

/// Parse the `[Desktop Entry]` group of a `.desktop` file (ignoring other
/// groups like `[Desktop Action …]` and localized `Name[xx]=` keys).
#[cfg(target_os = "linux")]
fn parse_desktop_entry(content: &str) -> DesktopEntry {
    let mut de = DesktopEntry::default();
    let mut in_entry = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Type" => de.entry_type = Some(val.trim().to_string()),
            // Unlocalized Name only — `Name[fr]` falls through to `_`.
            "Name" => de.name = Some(val.trim().to_string()),
            "MimeType" => {
                de.mimetypes = val
                    .split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            }
            "NoDisplay" => de.no_display = val.trim().eq_ignore_ascii_case("true"),
            "Hidden" => de.hidden = val.trim().eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    de
}

/// Scan the XDG application directories for visible `Application` entries that
/// declare `mime`, marking the one whose `.desktop` id equals `default_id`.
/// Earlier directories win on duplicate ids (user overrides system). Sorted
/// default-first, then by name. Pure `std` so it is unit-testable without a
/// desktop environment.
#[cfg(target_os = "linux")]
fn candidates_for_mime(mime: &str, default_id: &str) -> Vec<OpenWithCandidate> {
    use std::collections::HashSet;

    let mut out: Vec<OpenWithCandidate> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for dir in xdg_application_dirs() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Some(id) = p.file_name().and_then(|s| s.to_str()).map(str::to_string) else {
                continue;
            };
            if seen.contains(&id) {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&p) else {
                continue;
            };
            let de = parse_desktop_entry(&content);
            if de.entry_type.as_deref() != Some("Application")
                || de.no_display
                || de.hidden
                || !de.mimetypes.iter().any(|m| m == mime)
            {
                continue;
            }
            let Some(name) = de.name else {
                continue;
            };
            let is_default = id == default_id;
            seen.insert(id);
            out.push(OpenWithCandidate { name, path: p, is_default });
        }
    }

    out.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// Open `target` with a specific application. If `app_path` is a `.desktop`
/// entry we launch it through `gio launch` (which applies the entry's `Exec`
/// rules); otherwise we treat `app_path` as an executable and pass the file as
/// its first argument. Spawned detached.
#[cfg(target_os = "linux")]
pub fn open_with_app(target: &Path, app_path: &Path) -> Result<(), String> {
    let mut cmd = if app_path.extension().and_then(|e| e.to_str()) == Some("desktop") {
        let mut c = std::process::Command::new("gio");
        c.arg("launch").arg(app_path).arg(target);
        c
    } else {
        let mut c = std::process::Command::new(app_path);
        c.arg(target);
        c
    };
    spawn_detached(&mut cmd).map_err(|e| e.to_string())
}
#[cfg(not(target_os = "linux"))]
pub fn open_with_app(_target: &Path, _app_path: &Path) -> Result<(), String> {
    Err("open_with_app: not implemented on this platform".into())
}

/// Open every `target` with the app at `app_path`. One detached spawn
/// per file; the batch form exists for parity with shell-mac's
/// single-invocation `open -a`.
pub fn open_with_app_many(targets: &[std::path::PathBuf], app_path: &Path) -> Result<(), String> {
    let mut last_err = None;
    for target in targets {
        if let Err(e) = open_with_app(target, app_path) {
            last_err = Some(e);
        }
    }
    match last_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// =============================================================
// Clipboard
// =============================================================

/// Place plain text on the system clipboard (used by the copyable error
/// toast, "copy path", etc.). `arboard` handles both Wayland and X11.
#[cfg(target_os = "linux")]
pub fn copy_to_clipboard(text: &str) {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text.to_owned());
    }
}

#[cfg(not(target_os = "linux"))]
pub fn copy_to_clipboard(_text: &str) {}

// =============================================================
// Clipboard (file URLs)
// =============================================================

/// Copy file paths to the clipboard as `text/uri-list` `file://` URIs (plus
/// the GNOME `x-special/gnome-copied-files` target for Nautilus interop).
/// Stub — needs Wayland/X11 selection access (`wl-clipboard` / `xclip` / a
/// native protocol client). Returns `false` so callers surface "not
/// available" instead of a lying success toast (the `is_dir` half of
/// each item is a mac-pasteboard need).
pub fn clipboard_copy_file_urls(_items: &[(&Path, bool)]) -> bool {
    false
}

/// Read file paths previously placed on the clipboard. Empty if none. Stub.
pub fn clipboard_read_file_urls() -> Vec<PathBuf> {
    Vec::new()
}

// =============================================================
// File operations
// =============================================================

/// Duplicate a path in place. Mirrors the win32 `std::fs` implementation with a
/// Linux-flavoured copy-name convention: `stem (copy).ext`, then
/// `stem (copy N).ext`. Directories are copied recursively; symlinks inside a
/// directory are skipped (never followed — avoids cycles and dangling-link
/// copy errors).
#[cfg(target_os = "linux")]
pub fn duplicate_path(src: &Path) -> Result<PathBuf, String> {
    let parent = src
        .parent()
        .ok_or_else(|| format!("no parent dir for {}", src.display()))?;
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("unsupported filename for {}", src.display()))?;
    let ext = src
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{s}"))
        .unwrap_or_default();

    for n in 1..=99 {
        let candidate = if n == 1 {
            parent.join(format!("{stem} (copy){ext}"))
        } else {
            parent.join(format!("{stem} (copy {n}){ext}"))
        };
        if candidate.exists() {
            continue;
        }
        let metadata = std::fs::metadata(src).map_err(|e| e.to_string())?;
        if metadata.is_dir() {
            copy_dir_recursive(src, &candidate).map_err(|e| e.to_string())?;
        } else {
            std::fs::copy(src, &candidate).map_err(|e| e.to_string())?;
        }
        return Ok(candidate);
    }
    Err("duplicate_path: exhausted ' (copy 1..99)' slots".into())
}
#[cfg(not(target_os = "linux"))]
pub fn duplicate_path(_src: &Path) -> Result<PathBuf, String> {
    Err("duplicate_path: not implemented on this platform".into())
}

/// Unmount and eject the volume mounted at `path`, Finder-style:
/// unmount the filesystem, then — once nothing else on the same drive
/// is mounted — power the drive down so it's safe to unplug.
///
/// `udisksctl` does the heavy lifting (the desktop-standard udisks2
/// path; works without root for seat-local users), with a plain
/// `umount` fallback for setups without udisks. Synchronous — callers
/// dispatch from a worker. The power-off step is best-effort: by then
/// the volume the caller named is already unmounted.
#[cfg(target_os = "linux")]
pub fn eject_volume(path: &Path) -> Result<(), String> {
    eject_device(&[path])
}

#[cfg(not(target_os = "linux"))]
pub fn eject_volume(_path: &Path) -> Result<(), String> {
    Err("eject_volume: not implemented on this platform".into())
}

/// Unmount every volume in `volume_paths` (mount points on one physical
/// device), then power the device down — Finder's "Eject All". A single
/// path is the plain eject. If any unmount fails, the power-off is
/// skipped and the first error returned (already-unmounted siblings
/// stay unmounted, like Finder).
#[cfg(target_os = "linux")]
pub fn eject_device(volume_paths: &[&Path]) -> Result<(), String> {
    if volume_paths.is_empty() {
        return Err("eject: no volumes given".into());
    }
    let mut first_err: Option<String> = None;
    let mut disk: Option<String> = None;
    for path in volume_paths {
        let Some(source) = mount_source_for(path) else {
            first_err
                .get_or_insert_with(|| format!("no mounted filesystem found at {}", path.display()));
            continue;
        };
        if disk.is_none() {
            disk = parent_disk_of(&source);
        }
        if let Err(e) = unmount_filesystem(&source, path) {
            first_err.get_or_insert(e);
        }
    }
    if let Some(e) = first_err {
        return Err(e);
    }
    // Power off the drive once its last filesystem is gone, so the
    // "safe to unplug" semantics match Finder. Best-effort: by now the
    // volumes the caller named are already unmounted.
    if let Some(disk) = disk {
        if !disk_has_mounted_filesystems(&disk) {
            let _ = run_checked(std::process::Command::new("udisksctl").args([
                "power-off",
                "--no-user-interaction",
                "-b",
                &format!("/dev/{disk}"),
            ]));
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn eject_device(_volume_paths: &[&Path]) -> Result<(), String> {
    Err("eject_device: not implemented on this platform".into())
}

/// Unmount one filesystem: udisksctl first (the desktop-standard path),
/// plain `umount` as fallback for setups without udisks. When both
/// fail, surface the udisks error — it's the more descriptive one
/// ("target is busy", polkit denial, …).
#[cfg(target_os = "linux")]
fn unmount_filesystem(source: &str, mount_point: &Path) -> Result<(), String> {
    if let Err(udisks_err) = run_checked(
        std::process::Command::new("udisksctl")
            .args(["unmount", "--no-user-interaction", "-b", source]),
    ) {
        run_checked(std::process::Command::new("umount").arg(mount_point))
            .map_err(|_| udisks_err)?;
    }
    Ok(())
}

/// Names of processes holding files open on the volume at `path` — the
/// "why won't it eject" answer for a failed unmount. Scans
/// `/proc/<pid>/fd/*` and `/proc/<pid>/cwd` symlinks for targets inside
/// the mount point (what `lsof` does); without root this only sees the
/// user's own processes, which are exactly the ones a desktop eject
/// trips over. Best-effort, sorted, deduped, capped at 5. Callers
/// dispatch from a worker.
#[cfg(target_os = "linux")]
pub fn volume_busy_processes(path: &Path) -> Vec<String> {
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for entry in procs.flatten() {
        let pid_os = entry.file_name();
        let Some(pid) = pid_os
            .to_str()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        let proc_dir = entry.path();
        let mut holds =
            std::fs::read_link(proc_dir.join("cwd")).is_ok_and(|cwd| cwd.starts_with(path));
        if !holds {
            if let Ok(fds) = std::fs::read_dir(proc_dir.join("fd")) {
                holds = fds
                    .flatten()
                    .filter_map(|fd| std::fs::read_link(fd.path()).ok())
                    .any(|target| target.starts_with(path));
            }
        }
        if holds {
            let name = std::fs::read_to_string(proc_dir.join("comm"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            names.push(if name.is_empty() { format!("pid {pid}") } else { name });
        }
    }
    names.sort();
    names.dedup();
    names.truncate(5);
    names
}

#[cfg(not(target_os = "linux"))]
pub fn volume_busy_processes(_path: &Path) -> Vec<String> {
    Vec::new()
}

/// Run a command to completion; `Err` carries trimmed stderr (or the
/// spawn error) so eject failures surface *why* — "target is busy",
/// polkit denials — instead of a bare exit status.
#[cfg(target_os = "linux")]
fn run_checked(cmd: &mut std::process::Command) -> Result<(), String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    match cmd.output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr = stderr.trim();
            if stderr.is_empty() {
                Err(format!("{program} failed ({})", out.status))
            } else {
                Err(stderr.to_string())
            }
        }
        Err(e) => Err(format!("{program}: {e}")),
    }
}

/// The `/dev/...` source of the filesystem mounted at `path`, from
/// `/proc/self/mounts`. `None` for virtual/network sources.
#[cfg(target_os = "linux")]
fn mount_source_for(path: &Path) -> Option<String> {
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    let want = path.to_str()?;
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(source), Some(mount_point)) = (fields.next(), fields.next()) else {
            continue;
        };
        if unescape_mounts(mount_point) == want && source.starts_with("/dev/") {
            return Some(source.to_string());
        }
    }
    None
}

/// Parent disk name for a mount source: "/dev/sdb1" → "sdb",
/// "/dev/nvme0n1p2" → "nvme0n1". Resolved through sysfs (a partition's
/// `/sys/class/block/<name>` entry lives inside its disk's directory);
/// a whole-disk source maps to itself. `None` when unsure — safer to
/// skip the power-off than to power off the wrong drive.
#[cfg(target_os = "linux")]
fn parent_disk_of(source: &str) -> Option<String> {
    let name = source.strip_prefix("/dev/")?;
    if name.contains('/') {
        return None; // /dev/mapper/… — don't guess.
    }
    let sys = Path::new("/sys/class/block").join(name);
    if sys.join("partition").exists() {
        let target = std::fs::read_link(&sys).ok()?;
        let disk = target.parent()?.file_name()?.to_str()?;
        if disk == "block" {
            return None;
        }
        return Some(disk.to_string());
    }
    sys.exists().then(|| name.to_string())
}

/// True when any filesystem is still mounted from a partition (or the
/// whole device) of `disk` per `/proc/self/mounts` — gates the
/// post-unmount `power-off`.
#[cfg(target_os = "linux")]
fn disk_has_mounted_filesystems(disk: &str) -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else {
        return true; // Unknown → don't power off.
    };
    mounts
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|source| source.starts_with("/dev/"))
        .any(|source| parent_disk_of(source).as_deref() == Some(disk))
}

/// Decode the octal escapes `/proc/self/mounts` uses in path fields
/// (space `\040`, tab `\011`, newline `\012`, backslash `\134`).
#[cfg(target_os = "linux")]
fn unescape_mounts(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            if let Ok(n) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(n as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Make an "alias" to `target` beside it — a POSIX symlink named
/// `stem (link).ext` (next free `stem (link N).ext`). The macOS alias / Windows
/// `.lnk` concept maps cleanly to a symlink here.
#[cfg(target_os = "linux")]
pub fn make_alias(target: &Path) -> Result<PathBuf, String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("no parent dir for {}", target.display()))?;
    make_alias_in(target, parent)
}
#[cfg(not(target_os = "linux"))]
pub fn make_alias(_target: &Path) -> Result<PathBuf, String> {
    Err("make_alias: not implemented on this platform".into())
}

/// Make a symlink to `target` inside `dest_dir`, named `stem (link).ext` with a
/// `(link N)` suffix if taken (capped at 99). The link points at `target`'s
/// absolute path so it stays valid regardless of the reader's working dir.
#[cfg(target_os = "linux")]
pub fn make_alias_in(target: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    let stem = target
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("unsupported filename for {}", target.display()))?;
    let ext = target
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{s}"))
        .unwrap_or_default();

    // Point the link at an absolute path so it resolves from anywhere.
    let link_target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(target)
    };

    for n in 1..=99 {
        let candidate = if n == 1 {
            dest_dir.join(format!("{stem} (link){ext}"))
        } else {
            dest_dir.join(format!("{stem} (link {n}){ext}"))
        };
        // `symlink_metadata` so an existing *dangling* link still counts as taken.
        if candidate.symlink_metadata().is_ok() {
            continue;
        }
        std::os::unix::fs::symlink(&link_target, &candidate).map_err(|e| e.to_string())?;
        return Ok(candidate);
    }
    Err("make_alias_in: exhausted ' (link 1..99)' slots".into())
}
#[cfg(not(target_os = "linux"))]
pub fn make_alias_in(_target: &Path, _dest_dir: &Path) -> Result<PathBuf, String> {
    Err("make_alias_in: not implemented on this platform".into())
}

/// Compress `targets` into a `.zip` written next to the first target's parent.
/// Single source → `<filename>.zip`; multiple → `Archive.zip`; next free
/// `(N)` suffix if taken (capped at 99). Deflate at the `zip` crate's default
/// level. Directories are walked recursively; symlinks are skipped (a link
/// cycle would otherwise grow the walk forever, and archiving link targets via
/// their parent double-stores content). Platform-neutral — identical to the
/// win32 implementation. Call on a worker thread; large archives take seconds.
pub fn compress_paths(targets: &[&Path]) -> Result<PathBuf, String> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    if targets.is_empty() {
        return Err("compress_paths: no targets".into());
    }

    let parent = targets[0]
        .parent()
        .ok_or_else(|| "compress_paths: first target has no parent".to_string())?;
    let base = if targets.len() == 1 {
        targets[0]
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| "Archive".to_string())
    } else {
        "Archive".to_string()
    };

    // Choose unused zip path: <base>.zip, then <base> (2).zip, ...
    let mut out_path = parent.join(format!("{base}.zip"));
    if out_path.exists() {
        let mut chosen = None;
        for n in 2..=99 {
            let candidate = parent.join(format!("{base} ({n}).zip"));
            if !candidate.exists() {
                chosen = Some(candidate);
                break;
            }
        }
        out_path = chosen.ok_or_else(|| "compress_paths: name slots exhausted".to_string())?;
    }

    let file = std::fs::File::create(&out_path).map_err(|e| format!("create zip: {e}"))?;
    let mut writer = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for src in targets {
        let arc_base = src
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("compress_paths: invalid filename for {}", src.display()))?;
        if src.is_dir() {
            walk_into_zip(&mut writer, src, arc_base, opts)?;
        } else {
            writer
                .start_file(arc_base, opts)
                .map_err(|e| format!("zip start_file: {e}"))?;
            let bytes = std::fs::read(src).map_err(|e| format!("read {}: {e}", src.display()))?;
            writer
                .write_all(&bytes)
                .map_err(|e| format!("zip write: {e}"))?;
        }
    }
    writer.finish().map_err(|e| format!("zip finish: {e}"))?;
    Ok(out_path)
}

fn walk_into_zip(
    writer: &mut zip::ZipWriter<std::fs::File>,
    src_dir: &Path,
    arc_prefix: &str,
    opts: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    use std::io::Write;
    let mut stack: Vec<(PathBuf, String)> = vec![(src_dir.to_path_buf(), arc_prefix.to_string())];
    while let Some((dir, arc)) = stack.pop() {
        writer
            .add_directory(format!("{arc}/"), opts)
            .map_err(|e| format!("zip add_directory: {e}"))?;
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| format!("compress_paths: invalid filename in {}", dir.display()))?;
            let arc_path = format!("{arc}/{name}");
            let ft = entry.file_type().map_err(|e| format!("file_type: {e}"))?;
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push((path, arc_path));
            } else if ft.is_file() {
                writer
                    .start_file(&arc_path, opts)
                    .map_err(|e| format!("zip start_file: {e}"))?;
                let bytes =
                    std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
                writer
                    .write_all(&bytes)
                    .map_err(|e| format!("zip write: {e}"))?;
            }
        }
    }
    Ok(())
}

// =============================================================
// Quick Look equivalent (preview / thumbnails)
// =============================================================

/// Pop a system preview for `paths`. No Quick Look on Linux — route to the
/// in-app preview pane (or optional `sushi` shell-out). Stub.
pub fn show_quick_look(_paths: &[&Path]) -> Result<(), String> {
    Err("show_quick_look: not available on Linux".into())
}

/// Fetch a content thumbnail as straight RGBA8 `(bytes, width, height)` — the
/// same contract as `fetch_icon_rgba` (the gpui side swaps to BGRA when it
/// builds the `RenderImage`).
///
/// Rides the **shared freedesktop thumbnail cache** (`$XDG_CACHE_HOME/
/// thumbnails/{normal,large,x-large,xx-large}/<md5(file-uri)>.png`) so a
/// thumbnail Nautilus already generated returns instantly from disk — and a
/// thumbnail we generate is reusable by other file managers. On a miss (or a
/// stale entry — the source is newer than the cached PNG) it regenerates with
/// `gdk-pixbuf-thumbnailer`, which writes the spec `Thumb::*` tEXt chunks.
///
/// v1 covers what gdk-pixbuf loads (images). Video poster frames and PDF first
/// pages need their own thumbnailers (totem/evince) or the Tumbler D-Bus
/// service that dispatches to all registered thumbnailers — a follow-up; those
/// simply return `None` here and fall back to the type icon.
///
/// Off the render path (the gpui thumbnail warmer runs this on the background
/// pool), so the process spawn + disk I/O are Prime-Directive-safe.
#[cfg(target_os = "linux")]
pub fn fetch_quick_look_thumbnail(path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let (bucket, dim) = thumb_bucket(size_px);
    let digest = thumb_md5(&file_uri(path));
    let cache_path = thumbnails_cache_dir()?
        .join(bucket)
        .join(format!("{digest}.png"));

    // Reuse the cached PNG when it is at least as new as the source. This is a
    // cheaper stand-in for the spec's `Thumb::MTime` tEXt comparison: editing
    // the source bumps its mtime past the old thumbnail, forcing a regen.
    let fresh = cache_path
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .zip(meta.modified().ok())
        .is_some_and(|(thumb_mtime, src_mtime)| thumb_mtime >= src_mtime);

    if !fresh {
        std::fs::create_dir_all(cache_path.parent()?).ok()?;
        let ok = std::process::Command::new("gdk-pixbuf-thumbnailer")
            .arg("-s")
            .arg(dim.to_string())
            .arg(path)
            .arg(&cache_path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok || !cache_path.exists() {
            return None;
        }
    }
    decode_png_rgba(&cache_path)
}
#[cfg(not(target_os = "linux"))]
pub fn fetch_quick_look_thumbnail(_path: &Path, _size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    None
}

/// The freedesktop thumbnail cache root (`$XDG_CACHE_HOME/thumbnails`, else
/// `~/.cache/thumbnails`).
#[cfg(target_os = "linux")]
fn thumbnails_cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("thumbnails"))
}

/// Map a requested pixel size to the freedesktop cache bucket and its longest
/// edge (`normal`=128, `large`=256, `x-large`=512, `xx-large`=1024).
#[cfg(target_os = "linux")]
fn thumb_bucket(size_px: u32) -> (&'static str, u32) {
    match size_px {
        0..=128 => ("normal", 128),
        129..=256 => ("large", 256),
        257..=512 => ("x-large", 512),
        _ => ("xx-large", 1024),
    }
}

/// Lowercase-hex MD5 of the file URI — the freedesktop thumbnail cache key.
#[cfg(target_os = "linux")]
fn thumb_md5(uri: &str) -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(uri.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode a PNG to straight RGBA8 `(bytes, w, h)`.
#[cfg(target_os = "linux")]
fn decode_png_rgba(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::open(path).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Some((rgba.into_raw(), w, h))
}

// =============================================================
// Get Info — shell facts (the NSURL-resource-values equivalent)
// =============================================================

/// Shell-sourced Get Info facts. Shape mirrored from
/// `feraille_shell_mac::ShellInfo` so the Get Info panel reads one type
/// through the `platform_shell` alias on every OS. Linux has no per-file
/// UTI / "date added" / per-file hide-extension flag, so this is all
/// `None` for now (a future fill could derive `kind` from the shared MIME
/// database). The panel falls back to magic detection for the "Kind" row.
#[derive(Clone, Debug, Default)]
pub struct ShellInfo {
    pub uti: Option<String>,
    pub kind: Option<String>,
    pub added_unix: Option<i64>,
    pub hidden_extension: Option<bool>,
    pub is_package: Option<bool>,
    pub is_alias: Option<bool>,
}

/// Shell-sourced Get Info facts for `path`. Default (all `None`) on Linux.
/// Mirrors `feraille_shell_mac::read_shell_info`.
pub fn read_shell_info(_path: &Path) -> ShellInfo {
    ShellInfo::default()
}

/// Set the per-file "Hide extension" flag — a macOS Finder concept with no
/// Linux equivalent. Mirrors `feraille_shell_mac::set_hidden_extension`.
pub fn set_hidden_extension(_path: &Path, _hide: bool) -> Result<(), String> {
    Err("hiding the extension per-file is macOS-only".into())
}

// =============================================================
// Tags (Finder color tags) — no portable Linux equivalent
// =============================================================

/// Read the color tags on `path`. No portable native tag system on Linux;
/// candidate backing is private `feraille-meta` SQLite. Empty for now.
pub fn read_canonical_tags(_path: &Path) -> Vec<TagColor> {
    Vec::new()
}

/// Toggle a color tag on `path`. No-op stub (see `read_canonical_tags`).
pub fn toggle_tag(_path: &Path, _color: TagColor) -> Result<(), String> {
    Err("toggle_tag: not available on Linux".into())
}

// =============================================================
// Appearance / theme
// =============================================================

/// Whether the system prefers dark. v1 reads the GNOME
/// `org.gnome.desktop.interface color-scheme` gsetting (value contains
/// `prefer-dark` when dark). The portal `org.freedesktop.appearance` /
/// `color-scheme` is the cross-desktop upgrade once `ashpd` is wired.
#[cfg(target_os = "linux")]
pub fn system_is_dark() -> bool {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("dark"))
        .unwrap_or(false)
}
#[cfg(not(target_os = "linux"))]
pub fn system_is_dark() -> bool {
    false
}

/// Push the app's chosen appearance to the platform. No-op on Linux (the
/// compositor owns decorations; gpui themes itself).
pub fn set_app_appearance(_dark: bool) {}

/// Observe system light/dark changes. Real impl: subscribe to the portal
/// `SettingChanged` signal and fire `callback` (off-thread). Stub.
pub fn start_system_theme_observer(_callback: Box<dyn Fn(bool) + 'static>) {}

// =============================================================
// App identity / icon
// =============================================================

/// Swap the running app icon. **No-op on Linux** — icon identity comes from a
/// `.desktop` file + Wayland `app_id` / X11 `WM_CLASS`, not a runtime swap.
pub fn set_app_icon_from_png_bytes(_png_bytes: &[u8]) -> SetIconResult {
    SetIconResult::NotMacOs
}

/// Set the Windows AppUserModelID. **No-op on Linux** (taskbar-grouping is the
/// Wayland `app_id`'s job).
pub fn set_app_user_model_id(_id: &str) {}

/// The app bundle path. No bundle concept on Linux — `None`.
pub fn app_bundle_path() -> Option<String> {
    None
}

// =============================================================
// Volumes / power
// =============================================================

/// Observe volume mount/unmount. Real impl: udisks2 `InterfacesAdded/Removed`
/// signals (or `GVolumeMonitor`); call `callback` on each change. Stub.
pub fn start_volume_observer(_callback: Box<dyn Fn() + 'static>) {}

/// Observe power transitions (sleep/wake). Real impl: `org.freedesktop.login1`
/// `PrepareForSleep` D-Bus signal mapped to [`PowerEvent`]. Stub.
pub fn start_power_observer(_callback: Box<dyn Fn(PowerEvent) + 'static>) {}

/// Inhibit idle-triggered system sleep while the returned guard is held, via
/// `systemd-inhibit --what=idle`. systemd-inhibit holds the lock for the
/// lifetime of the command it wraps, so we wrap `sleep infinity` and keep that
/// child alive inside the [`SleepBlocker`]; dropping the guard kills it and
/// releases the lock. `--what=idle` mirrors macOS's "prevent idle system sleep"
/// (an explicit user suspend / lid close is still allowed). Returns `None` if
/// `systemd-inhibit` isn't available (non-systemd host) — callers treat that as
/// "couldn't inhibit", same as the macOS failure path.
#[cfg(target_os = "linux")]
pub fn prevent_idle_sleep(reason: &str) -> Option<SleepBlocker> {
    let child = std::process::Command::new("systemd-inhibit")
        .arg("--what=idle")
        .arg("--who=Feraille")
        .arg(format!("--why={reason}"))
        .arg("--mode=block")
        .arg("sleep")
        .arg("infinity")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    Some(SleepBlocker { child })
}
#[cfg(not(target_os = "linux"))]
pub fn prevent_idle_sleep(_reason: &str) -> Option<SleepBlocker> {
    None
}

// =============================================================
// Window
// =============================================================

/// Make a window float above others (viewer "always on top"). The
/// macOS-shaped `*mut c_void` (an `NSView`) is meaningless on Linux; a real
/// impl will key off the gpui/compositor handle instead. No-op for now.
pub fn set_window_floating(_handle: *mut c_void, _floating: bool) {}

/// Window docking primitives (docs/features/DOCK.md). macOS-only feature; the
/// `*mut c_void` handle is meaningless on Linux, so these are no-op stubs that
/// keep the shared `platform_shell::*` surface compiling. A real impl would
/// key off the Wayland/X11 compositor handle.
pub fn current_mouse_location() -> (f64, f64) {
    (0.0, 0.0)
}
pub fn screen_visible_frame_for_window(_handle: *mut c_void) -> Option<(f64, f64, f64, f64)> {
    None
}
pub fn set_window_frame(_handle: *mut c_void, _x: f64, _y: f64, _w: f64, _h: f64) {}
pub fn set_window_all_spaces(_handle: *mut c_void, _all_spaces: bool) {}
pub fn window_frame(_handle: *mut c_void) -> Option<(f64, f64, f64, f64)> {
    None
}
pub fn window_is_fullscreen(_handle: *mut c_void) -> bool {
    false
}

// =============================================================
// Video overlay (windowless player feeding the viewer BGRA frames).
// Real impl: GStreamer / libmpv. Handle 0 = "no video" (matches win32).
// See docs/features/VIEWER.md.
// =============================================================

/// Start a windowless video for `path`; returns an opaque handle (0 = none).
pub fn video_overlay_show(_path: &Path, _on_ended: Box<dyn Fn() + 'static>) -> u64 {
    0
}

/// Copy the current frame as `(width, height, rgba)`.
pub fn video_overlay_copy_frame(_id: u64) -> Option<(u32, u32, Vec<u8>)> {
    None
}

/// Tear down a video overlay.
pub fn video_overlay_remove(_id: u64) {}

/// Pause / resume.
pub fn video_overlay_set_paused(_id: u64, _paused: bool) {}

/// Mute / unmute. Stub: the Linux shell has no native player (mpv handles
/// audio + mute when selected).
pub fn video_overlay_set_muted(_id: u64, _muted: bool) {}

/// Restart from the beginning.
pub fn video_overlay_restart(_id: u64) {}

/// `(current_seconds, duration_seconds)`.
pub fn video_overlay_time(_id: u64) -> (f64, f64) {
    (0.0, 0.0)
}

/// Natural `(width, height)` of the video in pixels.
pub fn video_overlay_natural_size(_id: u64) -> (f64, f64) {
    (0.0, 0.0)
}

/// Seek to an absolute time in seconds.
pub fn video_overlay_seek(_id: u64, _seconds: f64) {}

/// Step `frames` forward (positive) or back (negative).
pub fn video_overlay_step(_id: u64, _frames: i64) {}

// =============================================================
// Linux-only private helpers
// =============================================================

/// Spawn a child process fully detached from Feraille: no inherited stdio, and
/// the caller does not wait on it. Used for launchers (`xdg-open`, terminals,
/// apps) so a slow or chatty child never blocks the calling worker. Returns
/// `Ok` once the child is spawned. A small named thread `wait()`s the child in
/// the background so it doesn't linger as a zombie until app exit — launchers
/// like `xdg-open` exit quickly, so the reaper threads are short-lived.
#[cfg(target_os = "linux")]
fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<()> {
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Best-effort: if the reaper thread can't start, the child still ran —
    // it just won't be reaped until process exit (the old behavior).
    let _ = std::thread::Builder::new()
        .name("child-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}

/// Build a `file://` URI for an absolute path. Minimal percent-encoding of the
/// characters that would otherwise break a URI (space, `%`, `#`, `?`). Good
/// enough for `FileManager1.ShowItems`; a full RFC 3986 encoder can replace it
/// when more shell surfaces need URIs.
#[cfg(target_os = "linux")]
fn file_uri(path: &Path) -> String {
    // Percent-encode from the RAW path bytes. The previous version
    // pushed each byte ≥ 0x80 as a `char` — mapping UTF-8 bytes to
    // Latin-1 codepoints that were then re-encoded as UTF-8, so
    // "Résumé.pdf" produced a double-encoded mojibake URI the file
    // manager couldn't resolve. Per RFC 3986, everything outside the
    // unreserved set is encoded; `/` stays as the path separator.
    #[cfg(unix)]
    let bytes: &[u8] = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let owned = path.to_string_lossy().into_owned();
    #[cfg(not(unix))]
    let bytes: &[u8] = owned.as_bytes();

    let mut out = String::from("file://");
    for &b in bytes {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Recursive directory copy used by [`duplicate_path`]. Skips symlinks (never
/// follows them) so a directory containing links can't induce a cycle or a
/// dangling-link copy error.
#[cfg(target_os = "linux")]
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

// Filesystem-backed tests for the pure-`std` operations.
//
// `compress_paths` is platform-neutral, so its tests are **not** gated and run
// on any host (`cargo test -p feraille-shell-linux` executes them on macOS).
// The `duplicate_path` / `make_alias_in` / `file_uri` tests are gated to Linux
// because the functions under test live behind `cfg(target_os = "linux")` —
// run them on a Linux host, or type-check with
// `cargo check --target x86_64-unknown-linux-gnu -p feraille-shell-linux --tests`.
#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway directory under the system temp dir, removed on drop.
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            // Unique without an rng dep: pid + a per-call atomic counter.
            use std::sync::atomic::{AtomicU32, Ordering};
            static SEQ: AtomicU32 = AtomicU32::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "feraille-shell-linux-{tag}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TmpDir(dir)
        }
        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Read back every entry name from a `.zip` (used by the compress tests).
    fn zip_entry_names(p: &Path) -> Vec<String> {
        let file = std::fs::File::open(p).unwrap();
        let mut ar = zip::ZipArchive::new(file).unwrap();
        (0..ar.len())
            .map(|i| ar.by_index(i).unwrap().name().to_string())
            .collect()
    }

    /// The `.desktop` scan core: visible `Application` entries declaring the
    /// MIME are offered (default first); hidden and non-matching entries are
    /// not. Hermetic — points XDG at temp dirs, no desktop environment needed.
    #[cfg(target_os = "linux")]
    #[test]
    fn open_with_candidates_scans_desktop_entries() {
        let data_home = TmpDir::new("openwith-home");
        let apps = data_home.join("applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join("myeditor.desktop"),
            "[Desktop Entry]\nType=Application\nName=My Editor\nName[fr]=Mon Éditeur\n\
             MimeType=text/plain;text/x-rust;\n",
        )
        .unwrap();
        std::fs::write(
            apps.join("hidden.desktop"),
            "[Desktop Entry]\nType=Application\nName=Hidden App\nMimeType=text/plain;\nNoDisplay=true\n",
        )
        .unwrap();
        std::fs::write(
            apps.join("imagetool.desktop"),
            "[Desktop Entry]\nType=Application\nName=Image Tool\nMimeType=image/png;\n",
        )
        .unwrap();

        // An empty second data dir so the default /usr/share fallback isn't scanned.
        let empty = TmpDir::new("openwith-empty");
        std::env::set_var("XDG_DATA_HOME", &data_home.0);
        std::env::set_var("XDG_DATA_DIRS", &empty.0);

        let cands = candidates_for_mime("text/plain", "myeditor.desktop");
        assert_eq!(cands.len(), 1, "expected just My Editor, got {cands:?}");
        assert_eq!(cands[0].name, "My Editor"); // unlocalized Name, not Name[fr]
        assert!(cands[0].is_default);
        assert_eq!(cands[0].path, apps.join("myeditor.desktop"));

        // A MIME no entry declares yields nothing.
        assert!(candidates_for_mime("application/x-nonesuch", "").is_empty());
    }

    #[test]
    fn compress_archives_file_and_directory_tree() {
        let tmp = TmpDir::new("zip");
        let loose = tmp.join("hello.txt");
        std::fs::write(&loose, b"hi").unwrap();
        let dir = tmp.join("proj");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.txt"), b"a").unwrap();

        // Multiple targets → "Archive.zip".
        let zip_path = compress_paths(&[loose.as_path(), dir.as_path()]).unwrap();
        assert_eq!(zip_path.file_name().unwrap(), "Archive.zip");

        let names = zip_entry_names(&zip_path);
        assert!(names.iter().any(|n| n == "hello.txt"), "names={names:?}");
        assert!(names.iter().any(|n| n == "proj/"), "names={names:?}");
        assert!(
            names.iter().any(|n| n == "proj/sub/a.txt"),
            "names={names:?}"
        );
    }

    #[test]
    fn compress_single_file_names_after_source_and_avoids_collision() {
        let tmp = TmpDir::new("zip1");
        let f = tmp.join("data.bin");
        std::fs::write(&f, b"x").unwrap();

        let first = compress_paths(&[f.as_path()]).unwrap();
        assert_eq!(first.file_name().unwrap(), "data.bin.zip");
        // A second run must not overwrite the first — it bumps to " (2)".
        let second = compress_paths(&[f.as_path()]).unwrap();
        assert_eq!(second.file_name().unwrap(), "data.bin (2).zip");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn duplicate_file_uses_copy_suffix_and_increments() {
        let tmp = TmpDir::new("dup");
        let src = tmp.join("notes.txt");
        std::fs::write(&src, b"hello").unwrap();

        let first = duplicate_path(&src).unwrap();
        assert_eq!(first.file_name().unwrap(), "notes (copy).txt");
        assert_eq!(std::fs::read(&first).unwrap(), b"hello");

        // Second duplicate must not clobber the first — it bumps to "(copy 2)".
        let second = duplicate_path(&src).unwrap();
        assert_eq!(second.file_name().unwrap(), "notes (copy 2).txt");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn duplicate_directory_copies_contents_recursively() {
        let tmp = TmpDir::new("dupdir");
        let src = tmp.join("proj");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/a.txt"), b"a").unwrap();

        let dup = duplicate_path(&src).unwrap();
        assert_eq!(dup.file_name().unwrap(), "proj (copy)");
        assert_eq!(std::fs::read(dup.join("sub/a.txt")).unwrap(), b"a");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn make_alias_in_creates_symlink_pointing_at_target() {
        let tmp = TmpDir::new("alias");
        let target = tmp.join("real.bin");
        std::fs::write(&target, b"x").unwrap();
        let dest = tmp.join("links");
        std::fs::create_dir_all(&dest).unwrap();

        let link = make_alias_in(&target, &dest).unwrap();
        assert_eq!(link.file_name().unwrap(), "real (link).bin");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(&link).unwrap(), target);

        // A second alias into the same dir gets the "(link 2)" suffix.
        let link2 = make_alias_in(&target, &dest).unwrap();
        assert_eq!(link2.file_name().unwrap(), "real (link 2).bin");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn file_uri_percent_encodes_spaces() {
        assert_eq!(file_uri(Path::new("/a b/c#d")), "file:///a%20b/c%23d");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn file_uri_encodes_non_ascii_from_raw_bytes() {
        // Each UTF-8 byte of 'é' (0xC3 0xA9) is percent-encoded; the
        // old byte→char push produced a double-encoded mojibake URI.
        assert_eq!(
            file_uri(Path::new("/tmp/Résumé.pdf")),
            "file:///tmp/R%C3%A9sum%C3%A9.pdf"
        );
    }

    // The thumbnail cache key must match what other file managers compute, or
    // we always-miss the shared cache. This is the canonical vector from the
    // freedesktop Thumbnail Managing Standard.
    #[cfg(target_os = "linux")]
    #[test]
    fn thumb_md5_matches_freedesktop_spec_vector() {
        assert_eq!(
            thumb_md5("file:///home/jens/photo/me.png"),
            "d40775e596682f2a16d1b834c221c0a2"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn thumb_bucket_maps_sizes_to_freedesktop_dirs() {
        assert_eq!(thumb_bucket(96), ("normal", 128));
        assert_eq!(thumb_bucket(128), ("normal", 128));
        assert_eq!(thumb_bucket(129), ("large", 256));
        assert_eq!(thumb_bucket(512), ("x-large", 512));
        assert_eq!(thumb_bucket(1024), ("xx-large", 1024));
    }
}

// ---------------------------------------------------------------------------
// Privilege escalation + locked-file diagnostics (resilient file operations).
//
// STUBS for now. Linux follow-up: pkexec/sudo re-exec for run_elevated_self;
// /proc/*/fd scan for processes_using; SIGTERM for force_close_processes.
// ---------------------------------------------------------------------------

/// A process holding a file open. Identical shape in every shell crate.
#[derive(Clone, Debug)]
pub struct LockingProcess {
    pub pid: u32,
    pub name: String,
}

pub fn elevation_available() -> bool {
    false
}

pub fn run_elevated_self(_args: &[String]) -> Result<i32, String> {
    Err("elevation not implemented on Linux yet (pkexec re-exec)".into())
}

pub fn lock_diagnostics_available() -> bool {
    false
}

pub fn processes_using(_path: &std::path::Path) -> Vec<LockingProcess> {
    Vec::new()
}

pub fn force_close_processes(_pids: &[u32]) -> Result<(), String> {
    Err("force-close not implemented on Linux yet".into())
}
