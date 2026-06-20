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
//! Implemented (pure `std` / process-based, no external deps yet):
//! `duplicate_path`, `make_alias`, `make_alias_in`, `open_url`,
//! `reveal_in_finder`, `open_terminal`, `system_is_dark`, `open_with_app`.
//! Everything else is still a stub (clipboard, trash, thumbnails, observers,
//! video — these need `ashpd`/`zbus`/`zip`/`gstreamer`).

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
#[cfg(target_os = "linux")]
pub fn reveal_in_finder(path: &Path) {
    let uri = file_uri(path);
    let shown = std::process::Command::new("dbus-send")
        .args([
            "--session",
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

/// Enumerate apps that can open `path`. Real impl: resolve MIME, list handler
/// `.desktop` files. Stub for now.
pub fn open_with_candidates(_path: &Path) -> Vec<OpenWithCandidate> {
    Vec::new()
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

// =============================================================
// Clipboard (file URLs)
// =============================================================

/// Copy file paths to the clipboard as `text/uri-list` `file://` URIs (plus
/// the GNOME `x-special/gnome-copied-files` target for Nautilus interop).
/// Stub — needs Wayland/X11 selection access (`wl-clipboard` / `xclip` / a
/// native protocol client).
pub fn clipboard_copy_file_urls(_paths: &[&Path]) {}

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

/// Eject / unmount a volume. Real impl: udisks2 `Filesystem.Unmount` /
/// `Drive.Eject` over D-Bus. Stub.
pub fn eject_volume(_path: &Path) -> Result<(), String> {
    Err("eject_volume: not implemented on Linux yet".into())
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

/// Fetch a thumbnail as `(rgba_or_png_bytes, width, height)`. Real impl: reuse
/// the freedesktop thumbnail cache (`$XDG_CACHE_HOME/thumbnails`), else
/// generate via gdk-pixbuf / Tumbler. Stub.
pub fn fetch_quick_look_thumbnail(_path: &Path, _size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    None
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
/// we do not wait on it. Used for launchers (`xdg-open`, terminals, apps) so a
/// slow or chatty child never blocks the calling worker. Returns `Ok` once the
/// child is spawned (we deliberately don't await its exit).
#[cfg(target_os = "linux")]
fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<()> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_child| ())
}

/// Build a `file://` URI for an absolute path. Minimal percent-encoding of the
/// characters that would otherwise break a URI (space, `%`, `#`, `?`). Good
/// enough for `FileManager1.ShowItems`; a full RFC 3986 encoder can replace it
/// when more shell surfaces need URIs.
#[cfg(target_os = "linux")]
fn file_uri(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::from("file://");
    for b in s.bytes() {
        match b {
            b'%' => out.push_str("%25"),
            b' ' => out.push_str("%20"),
            b'#' => out.push_str("%23"),
            b'?' => out.push_str("%3F"),
            _ => out.push(b as char),
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
}
