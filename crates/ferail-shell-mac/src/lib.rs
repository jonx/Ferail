//! macOS shell integration for Ferail.
//!
//! Iter-4.1 ships **native chrome**: transparent titlebar and
//! `.fullSizeContentView` so the tabstrip can sit alongside the
//! traffic-light buttons. Drag-drop (`NSPasteboard`), right-click
//! menus (`NSMenu`), vibrancy (`NSVisualEffectView`), and Quick Look
//! preview land in subsequent iterations.
//!
//! Non-macOS builds get no-op stubs so callers can use this crate
//! unconditionally.

#[cfg(target_os = "macos")]
mod app_menu;

#[cfg(target_os = "macos")]
mod archive;

#[cfg(target_os = "macos")]
mod file_ops;

#[cfg(target_os = "macos")]
mod file_promise;

#[cfg(target_os = "macos")]
pub use file_promise::FilePromise;

#[cfg(target_os = "macos")]
pub fn start_file_promise_drag(ns_view: *mut std::ffi::c_void, promises: Vec<FilePromise>) -> bool {
    file_promise::start(ns_view, promises)
}

#[cfg(target_os = "macos")]
mod open_with;

#[cfg(target_os = "macos")]
mod quick_look;

#[cfg(target_os = "macos")]
mod tags;

#[cfg(target_os = "macos")]
mod theme_observer;

#[cfg(target_os = "macos")]
mod volume_observer;

#[cfg(target_os = "macos")]
mod power_observer;

#[cfg(target_os = "macos")]
mod power_assert;

#[cfg(target_os = "macos")]
mod video_overlay;

/// Spotlight-backed global search (Tier 2). Cross-platform module with
/// internal cfg gates: non-macOS builds get unsupported stubs.
pub mod spotlight;
pub use spotlight::{spotlight_available, spotlight_search, SpotlightScope};

/// AppKit-sourced facts for the Get Info panel (UTI, localized Kind,
/// date-added, Finder attribute bits). Cross-platform module with internal
/// cfg gates: non-macOS builds get an empty record.
pub mod resource_values;
pub use resource_values::{read_shell_info, set_hidden_extension, ShellInfo};

/// Install the application menu bar (`NSApp.mainMenu`) and configure the
/// standard About panel content. Call once at startup, on the main thread,
/// after [`set_app_icon_from_png_bytes`]. No-op on non-macOS.
#[cfg(target_os = "macos")]
pub fn install_app_menu(app_name: &str, tagline: &str, version: &str, copyright: &str) {
    app_menu::install_app_menu(app_name, tagline, version, copyright);
}

#[cfg(not(target_os = "macos"))]
pub fn install_app_menu(_app_name: &str, _tagline: &str, _version: &str, _copyright: &str) {}

/// Register the host-app callback for Ferail-owned menu commands.
/// Fires on the main thread with the [`ferail_core::commands::CommandId`]
/// of the picked item. Pass `None` to clear. No-op on non-macOS.
#[cfg(target_os = "macos")]
pub fn register_command_callback(
    cb: Option<Box<dyn Fn(ferail_core::commands::CommandId) + 'static>>,
) {
    app_menu::register_command_callback(cb);
}

#[cfg(not(target_os = "macos"))]
pub fn register_command_callback(
    _cb: Option<Box<dyn Fn(ferail_core::commands::CommandId) + 'static>>,
) {
}

/// Show the standard About panel using the dictionary configured by
/// [`install_app_menu`] or [`set_about_options`]. The host app calls
/// this in response to the `app.about` command. No-op on non-macOS.
#[cfg(target_os = "macos")]
pub fn show_about_panel() {
    app_menu::show_about_panel();
}

#[cfg(not(target_os = "macos"))]
pub fn show_about_panel() {}

/// Populate the About-panel options dictionary without installing the
/// full menu bar. Useful when the host builds its menu through another
/// channel (e.g. gpui's `cx.set_menus`) but still wants
/// [`show_about_panel`] to display a populated dialog.
#[cfg(target_os = "macos")]
pub fn set_about_options(app_name: &str, tagline: &str, version: &str, copyright: &str) {
    app_menu::set_about_options(app_name, tagline, version, copyright);
}

#[cfg(not(target_os = "macos"))]
pub fn set_about_options(_app_name: &str, _tagline: &str, _version: &str, _copyright: &str) {}

/// Update the menu's snapshot of the host's tab count. Used by
/// `validateMenuItem:` to enable / disable `file.close_tab`. Call
/// from the host whenever the tab count changes (open / close /
/// initial state). No-op on non-macOS.
#[cfg(target_os = "macos")]
pub fn set_tab_count(n: usize) {
    app_menu::set_tab_count(n);
}

#[cfg(not(target_os = "macos"))]
pub fn set_tab_count(_n: usize) {}

/// Set whether a command's menu item should render a checkmark.
/// Used for radio-button-style exclusive groups: the host flips one
/// item to `true` and the rest to `false`. Picked up on the next
/// menu open. No-op on non-macOS or before [`install_app_menu`].
#[cfg(target_os = "macos")]
pub fn set_command_state(id: ferail_core::commands::CommandId, on: bool) {
    app_menu::set_command_state(id, on);
}

#[cfg(not(target_os = "macos"))]
pub fn set_command_state(_id: ferail_core::commands::CommandId, _on: bool) {}

/// Show a native modal NSAlert with the given title and body. Single
/// "OK" button, informational style. Used for any "show this text
/// in a sheet" surface the host wants: Settings placeholder, Help
/// shortcuts cheat sheet, etc. Host composes both strings.
#[cfg(target_os = "macos")]
pub fn show_alert(title: &str, body: &str) {
    use objc2_app_kit::{NSAlert, NSAlertStyle};
    use objc2_foundation::{MainThreadMarker, NSString};

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    unsafe {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(body));
        alert.setAlertStyle(NSAlertStyle::Informational);
        alert.addButtonWithTitle(&NSString::from_str(&ferail_core::tr!("OK")));
        let _ = alert.runModal();
    }
}

#[cfg(not(target_os = "macos"))]
pub fn show_alert(_title: &str, _body: &str) {}

/// Present a native folder picker (`NSOpenPanel` restricted to
/// directories) and return the chosen path, or `None` if the user
/// cancelled. Synchronous / modal like [`show_alert`]: must be called
/// on the main thread. Backs the Favorites "Locate…" repoint flow
/// (`docs/features/FAVORITES.md` §8.2 / §8.3).
#[cfg(target_os = "macos")]
pub fn pick_folder() -> Option<std::path::PathBuf> {
    use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::MainThreadMarker;

    let mtm = MainThreadMarker::new()?;
    unsafe {
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseFiles(false);
        panel.setCanChooseDirectories(true);
        panel.setAllowsMultipleSelection(false);
        panel.setResolvesAliases(true);
        if panel.runModal() != NSModalResponseOK {
            return None;
        }
        let url = panel.URL()?;
        let path = url.path()?;
        Some(std::path::PathBuf::from(path.to_string()))
    }
}

#[cfg(not(target_os = "macos"))]
pub fn pick_folder() -> Option<std::path::PathBuf> {
    None
}

/// Open `url` in the user's default handler (typically the browser).
/// Best-effort: failures are logged at the AppKit level and we don't
/// surface them. macOS uses NSWorkspace; non-macOS falls back to the
/// `open` / `xdg-open` shell command.
#[cfg(target_os = "macos")]
pub fn open_url(url: &str) {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};

    unsafe {
        let ns_str = NSString::from_str(url);
        let Some(ns_url) = NSURL::URLWithString(&ns_str) else {
            return;
        };
        let workspace = NSWorkspace::sharedWorkspace();
        let _ = workspace.openURL(&ns_url);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Place a string on the system clipboard.
#[cfg(target_os = "macos")]
pub fn copy_to_clipboard(text: &str) {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let ns_text = NSString::from_str(text);
        let _ = pb.setString_forType(&ns_text, NSPasteboardTypeString);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn copy_to_clipboard(_text: &str) {}

/// Filesystem path of the running application, suitable for pasting
/// into a macOS file picker (e.g. the Full Disk Access "+" sheet).
/// When launched from a `.app` bundle this is the bundle path; a bare
/// binary (e.g. `cargo run`) returns the executable path. `None` if
/// the path can't be determined. Cheap, no directory reads.
#[cfg(target_os = "macos")]
pub fn app_bundle_path() -> Option<String> {
    use objc2_foundation::NSBundle;
    unsafe {
        let bundle = NSBundle::mainBundle();
        let path = bundle.bundlePath();
        let s = path.to_string();
        // A loose binary reports the enclosing directory as its
        // "bundle"; prefer the real executable path in that case.
        if s.ends_with(".app") {
            return Some(s);
        }
    }
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(not(target_os = "macos"))]
pub fn app_bundle_path() -> Option<String> {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Spawn `cmd` detached: stdio is nulled so the child can never block on
/// inherited pipes, and a small named thread `wait()`s it so it doesn't
/// linger as a zombie until app exit. The children launched through here
/// (`open`, `qlmanage`) exit quickly, so the reaper threads are short-lived.
#[cfg(target_os = "macos")]
pub(crate) fn spawn_and_reap(cmd: &mut std::process::Command) -> std::io::Result<()> {
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Best-effort: if the reaper thread can't start, the child still ran:
    // it just won't be reaped until process exit (the old behavior).
    let _ = std::thread::Builder::new()
        .name("child-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}

/// Hand a filesystem item to its default application without blocking on the
/// launched child. Kept in the platform-shell surface so Windows can use
/// ShellExecute rather than inheriting a command-line implementation from the
/// filesystem crate.
#[cfg(target_os = "macos")]
pub fn open_with_default(path: &std::path::Path) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new("open");
    cmd.arg(path);
    spawn_and_reap(&mut cmd)
}

#[cfg(not(target_os = "macos"))]
pub fn open_with_default(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

/// Open one file specifically in TextEdit. This is an explicit editing verb,
/// distinct from `open_with_default` (which may choose a viewer or Ferail).
#[cfg(target_os = "macos")]
pub fn edit_text_file(path: &std::path::Path) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new("/usr/bin/open");
    cmd.arg("-a").arg("TextEdit").arg(path);
    spawn_and_reap(&mut cmd)
}

#[cfg(not(target_os = "macos"))]
pub fn edit_text_file(_path: &std::path::Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "TextEdit is unavailable on this platform",
    ))
}

/// Open Finder with `path` selected. macOS: shells out to `open -R`.
/// Non-macOS: no-op.
#[cfg(target_os = "macos")]
pub fn reveal_in_finder(path: &std::path::Path) {
    let _ = try_reveal_in_finder(path);
}

#[cfg(target_os = "macos")]
pub fn try_reveal_in_finder(path: &std::path::Path) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new("open");
    cmd.arg("-R").arg(path);
    spawn_and_reap(&mut cmd)
}

#[cfg(not(target_os = "macos"))]
pub fn reveal_in_finder(_path: &std::path::Path) {}

#[cfg(not(target_os = "macos"))]
pub fn try_reveal_in_finder(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

/// Open a terminal at `path` (a directory) with the default spec:
/// `open -a Terminal <dir>`. See [`open_terminal_with`].
pub fn open_terminal(path: &std::path::Path) {
    open_terminal_with(path, &ferail_core::terminal::TerminalSpec::default());
}

/// Open a terminal at `path` (a directory) per the user's terminal
/// preferences (docs/features/CONTEXT_MENU.md).
///
/// Standard mode:
/// - no custom program: `open -a Terminal <dir>` (today's behavior);
/// - a `.app` bundle: `open -a <app> [<dir>] [--args <params>]`, the
///   `<dir>` document passed only when the params never mention `{dir}`;
/// - a plain binary: spawned directly with the resolved params, working
///   directory set to `path` when `{dir}` isn't in the params.
///
/// Admin mode opens a **root shell** (there is no "launch a GUI app
/// elevated" on macOS): a CLI-binary terminal gets its exec flag +
/// `sudo -s`; anything else routes through Terminal.app via AppleScript
/// `do script "cd <dir> && sudo -s"`: the sudo password prompt appears
/// inside the terminal. The AppleScript path needs the one-time
/// Automation (TCC) consent to control Terminal. Callers run this on a
/// worker (Prime Directive): `osascript` can block on that consent.
#[cfg(target_os = "macos")]
pub fn open_terminal_with(path: &std::path::Path, spec: &ferail_core::terminal::TerminalSpec) {
    use ferail_core::terminal::{exec_prefix_for, POSIX_ADMIN_SHELL};

    let dir = path.to_string_lossy();
    let (args, had_dir) = spec.resolved_args(&dir);
    let program = spec.program();
    // `.app` paths and bare names ("iTerm") go through LaunchServices;
    // only a slash-path that isn't a bundle is treated as a CLI binary.
    let is_app_bundle = |p: &str| p.to_ascii_lowercase().ends_with(".app") || !p.contains('/');

    if spec.admin() {
        // A bare command name ("kitty") or path to a CLI binary can carry
        // the root shell itself; app bundles (and the Terminal.app
        // default) go through AppleScript.
        if let Some(p) =
            program.filter(|p| p.contains('/') && !p.to_ascii_lowercase().ends_with(".app"))
        {
            let mut cmd = std::process::Command::new(p);
            cmd.args(&args);
            cmd.current_dir(path);
            cmd.args(exec_prefix_for(p));
            cmd.args(POSIX_ADMIN_SHELL);
            let _ = spawn_and_reap(&mut cmd);
            return;
        }
        let shell_cmd = format!("cd {} && sudo -s", elevation::shell_quote(&dir));
        let script = format!(
            "tell application \"Terminal\"\nactivate\ndo script {}\nend tell",
            elevation::applescript_quote(&shell_cmd)
        );
        let mut cmd = std::process::Command::new("/usr/bin/osascript");
        cmd.arg("-e").arg(&script);
        let _ = spawn_and_reap(&mut cmd);
        return;
    }

    match program {
        None => {
            let mut cmd = std::process::Command::new("open");
            cmd.arg("-a").arg("Terminal").arg(path);
            let _ = spawn_and_reap(&mut cmd);
        }
        // `.app` bundles (or bare app names like "iTerm") launch through
        // LaunchServices so the running instance gets the open, not a
        // second copy.
        Some(p) if is_app_bundle(p) => {
            let mut cmd = std::process::Command::new("open");
            cmd.arg("-a").arg(p);
            if !had_dir {
                cmd.arg(path);
            }
            if !args.is_empty() {
                cmd.arg("--args");
                cmd.args(&args);
            }
            let _ = spawn_and_reap(&mut cmd);
        }
        Some(p) => {
            let mut cmd = std::process::Command::new(p);
            cmd.args(&args);
            if !had_dir {
                cmd.current_dir(path);
            }
            let _ = spawn_and_reap(&mut cmd);
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn open_terminal_with(_path: &std::path::Path, _spec: &ferail_core::terminal::TerminalSpec) {}

/// Duplicate `src` next to itself with Finder's " copy" / " copy 2"
/// naming. Returns the destination path on success, or an error
/// string on failure (collision exhausted, IO error). Synchronous:
/// callers run this on a worker.
#[cfg(target_os = "macos")]
pub fn duplicate_path(src: &std::path::Path) -> Result<std::path::PathBuf, String> {
    file_ops::duplicate(src)
}

#[cfg(not(target_os = "macos"))]
pub fn duplicate_path(_src: &std::path::Path) -> Result<std::path::PathBuf, String> {
    Err("duplicate is macOS-only in this build".into())
}

/// Unmount and eject the volume mounted at `path`. Synchronous:
/// callers run this on a worker. Non-macOS: error.
#[cfg(target_os = "macos")]
pub fn eject_volume(path: &std::path::Path) -> Result<(), String> {
    file_ops::eject_volume(path)
}

#[cfg(not(target_os = "macos"))]
pub fn eject_volume(_path: &std::path::Path) -> Result<(), String> {
    Err("eject is macOS-only in this build".into())
}

/// Unmount every volume on the physical device backing
/// `volume_paths[0]` and eject the device (Finder's "Eject All").
/// Synchronous: callers run this on a worker. Non-macOS: error.
#[cfg(target_os = "macos")]
pub fn eject_device(volume_paths: &[&std::path::Path]) -> Result<(), String> {
    file_ops::eject_device(volume_paths)
}

#[cfg(not(target_os = "macos"))]
pub fn eject_device(_volume_paths: &[&std::path::Path]) -> Result<(), String> {
    Err("eject is macOS-only in this build".into())
}

/// Processes holding files open on the volume at `path`: the
/// "why won't it eject" answer for a failed eject, with pids so the UI
/// can activate the blocking app ([`activate_app`]). Synchronous:
/// callers run this on a worker. Non-macOS: empty.
#[cfg(target_os = "macos")]
pub fn volume_busy_processes(path: &std::path::Path) -> Vec<ferail_core::BusyApp> {
    file_ops::volume_busy_processes(path)
}

/// Bring the application owning `pid` to the foreground: the click
/// action on a failed-eject toast's culprit chips, so the user can go
/// close the files blocking the eject. `false` when the pid has no GUI
/// application to activate (a daemon or a shell), which callers treat
/// as a no-op. Cheap AppKit lookup; call from the UI thread.
#[cfg(target_os = "macos")]
pub fn activate_app(pid: i32) -> bool {
    use objc2::runtime::AnyObject;
    unsafe {
        let app: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        if app.is_null() {
            return false;
        }
        // NSApplicationActivateAllWindows | NSApplicationActivateIgnoringOtherApps.
        objc2::msg_send![app, activateWithOptions: 3usize]
    }
}

#[cfg(not(target_os = "macos"))]
pub fn activate_app(_pid: i32) -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn volume_busy_processes(_path: &std::path::Path) -> Vec<ferail_core::BusyApp> {
    Vec::new()
}

/// Make a Finder-resolvable alias file pointing at `target`.
/// Synchronous: callers run this on a worker.
#[cfg(target_os = "macos")]
pub fn make_alias(target: &std::path::Path) -> Result<std::path::PathBuf, String> {
    file_ops::make_alias(target)
}

#[cfg(not(target_os = "macos"))]
pub fn make_alias(_target: &std::path::Path) -> Result<std::path::PathBuf, String> {
    Err("make_alias is macOS-only".into())
}

/// Make a Finder-resolvable alias to `target` inside `dest_dir` (used by
/// Cmd+Option alias-drop). Synchronous: callers run this on a worker.
#[cfg(target_os = "macos")]
pub fn make_alias_in(
    target: &std::path::Path,
    dest_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    file_ops::make_alias_in(target, dest_dir)
}

#[cfg(not(target_os = "macos"))]
pub fn make_alias_in(
    _target: &std::path::Path,
    _dest_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    Err("make_alias is macOS-only".into())
}

/// Compress `targets` into a `.zip` next to the first target's
/// parent (Finder behaviour: `Foo.zip` for one source, `Archive.zip`
/// for several). Synchronous: callers run this on a worker.
#[cfg(target_os = "macos")]
pub fn compress_paths(targets: &[&std::path::Path]) -> Result<std::path::PathBuf, String> {
    archive::compress(targets)
}

#[cfg(not(target_os = "macos"))]
pub fn compress_paths(_targets: &[&std::path::Path]) -> Result<std::path::PathBuf, String> {
    Err("compress is macOS-only in this build".into())
}

/// Open Quick Look on `paths`. Stage B uses the standalone
/// `qlmanage -p` window so we don't need responder-chain plumbing
/// for `QLPreviewPanel`. Non-blocking: spawns and detaches.
#[cfg(target_os = "macos")]
pub fn show_quick_look(paths: &[&std::path::Path]) -> Result<(), String> {
    quick_look::show(paths)
}

/// Generate a Quick Look thumbnail for `path` at `size_px` (longest
/// edge). Returns RGBA8888 bytes plus the actual image dimensions.
/// Synchronous: call from a worker thread. macOS-only; non-macOS
/// returns `None`.
#[cfg(target_os = "macos")]
pub fn fetch_quick_look_thumbnail(
    path: &std::path::Path,
    size_px: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    quick_look::fetch_thumbnail(path, size_px)
}

#[cfg(not(target_os = "macos"))]
pub fn fetch_quick_look_thumbnail(
    _path: &std::path::Path,
    _size_px: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    None
}

/// The preview pane's fetch. Quick Look already renders document
/// content (page 1 of a PDF, a Pages/Word file, …), so on macOS this is
/// the same call as [`fetch_quick_look_thumbnail`]; Windows separates the
/// two tiers because its shell does not.
pub fn fetch_preview_image(path: &std::path::Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    fetch_quick_look_thumbnail(path, size_px)
}

/// Last-resort large type image: macOS callers draw their own type
/// glyphs when every content tier fails, so there is nothing to add here.
pub fn fetch_type_icon(_path: &std::path::Path, _size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn show_quick_look(_paths: &[&std::path::Path]) -> Result<(), String> {
    Err("quick_look is macOS-only".into())
}

/// Whether the active platform shell has a native, writable tag store.
pub const SUPPORTS_TAGS: bool = cfg!(target_os = "macos");

pub fn prefer_installer_updates() -> bool {
    false
}

pub fn launch_update_installer(_path: &std::path::Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Windows installer unavailable",
    ))
}

/// Read the canonical Finder-colour tags currently set on `path`.
/// User-defined tag names are dropped on the floor here; callers
/// that need the raw strings should use `read_tag_names` instead.
#[cfg(target_os = "macos")]
pub fn read_canonical_tags(path: &std::path::Path) -> Vec<ferail_core::commands::TagColor> {
    tags::read_canonical_tags(path)
}

#[cfg(not(target_os = "macos"))]
pub fn read_canonical_tags(_path: &std::path::Path) -> Vec<ferail_core::commands::TagColor> {
    Vec::new()
}

/// Toggle a single Finder colour tag on `path`. Other tags
/// (including user-defined ones) are preserved. Synchronous:
/// callers run this on a worker if the selection is large.
#[cfg(target_os = "macos")]
pub fn toggle_tag(
    path: &std::path::Path,
    color: ferail_core::commands::TagColor,
) -> Result<(), String> {
    tags::toggle_tag(path, color)
}

#[cfg(not(target_os = "macos"))]
pub fn toggle_tag(
    _path: &std::path::Path,
    _color: ferail_core::commands::TagColor,
) -> Result<(), String> {
    Err("toggle_tag is macOS-only".into())
}

/// Strip every tag (including user-defined ones) from `path`.
#[cfg(target_os = "macos")]
pub fn clear_tags(path: &std::path::Path) -> Result<(), String> {
    tags::write_tags(path, &[])
}

#[cfg(not(target_os = "macos"))]
pub fn clear_tags(_path: &std::path::Path) -> Result<(), String> {
    Err("clear_tags is macOS-only".into())
}

/// One app the system would offer in the Open With submenu for a
/// given file. `is_default` flags the system's preferred handler;
/// the right-click builder pins it to the top.
#[derive(Clone, Debug)]
pub struct OpenWithCandidate {
    pub name: String,
    pub path: std::path::PathBuf,
    pub is_default: bool,
}

#[cfg(target_os = "macos")]
impl From<open_with::OpenWithCandidate> for OpenWithCandidate {
    fn from(c: open_with::OpenWithCandidate) -> Self {
        OpenWithCandidate {
            name: c.name,
            path: c.path,
            is_default: c.is_default,
        }
    }
}

/// Enumerate the apps Launch Services would offer to open `path`.
/// Synchronous (~10–50 ms typical). Empty on failure.
#[cfg(target_os = "macos")]
pub fn open_with_candidates(path: &std::path::Path) -> Vec<OpenWithCandidate> {
    open_with::candidates_for(path)
        .into_iter()
        .map(Into::into)
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn open_with_candidates(_path: &std::path::Path) -> Vec<OpenWithCandidate> {
    Vec::new()
}

/// Open `target` with the app at `app_path`. Shells out to
/// `/usr/bin/open -a` so we don't have to wire up the
/// `NSWorkspace.openURLs:` completion-handler contract.
///
/// `open` waits for the target app to check in: seconds on a cold
/// launch, so call from a worker, never the UI thread.
pub fn open_with_app(target: &std::path::Path, app_path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        open_with::open_with(target, app_path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (target, app_path);
        Err("open_with_app is macOS-only".into())
    }
}

/// Open every `target` with the app at `app_path` in ONE
/// `/usr/bin/open -a` invocation: N sequential `open` calls each wait
/// for the app to check in, so a multi-selection pays the launch wait
/// once instead of N times. Same worker-only contract as
/// [`open_with_app`].
pub fn open_with_app_many(
    targets: &[std::path::PathBuf],
    app_path: &std::path::Path,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        open_with::open_with_many(targets, app_path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (targets, app_path);
        Err("open_with_app_many is macOS-only".into())
    }
}

/// Replace the running process's dock/app icon with the image decoded
/// from `png_bytes`. Useful when the binary is launched outside an .app
/// bundle (cargo run, debugger) where macOS would otherwise show the
/// generic executable icon.
///
/// Silently no-ops on non-macOS, and on macOS if the PNG can't be
/// decoded into an `NSImage`. Must be called on the main thread (the
/// usual app-startup constraint).
/// Result of [`set_app_icon_from_png_bytes`], for callers that want to
/// log success vs. failure.
#[derive(Debug)]
pub enum SetIconResult {
    /// Icon was decoded and assigned to NSApplication.
    Ok,
    /// Not on macOS; no-op stub.
    NotMacOs,
    /// Called off the main thread; refused.
    NotMainThread,
    /// `NSImage initWithData:` returned nil: PNG decode failed.
    DecodeFailed,
}

/// Replace the running process's dock/app icon with the image decoded
/// from `png_bytes`. Useful when the binary is launched outside an .app
/// bundle (cargo run, debugger) where macOS would otherwise show the
/// generic executable icon.
///
/// Must be called on the main thread, **after** winit has built its
/// event loop (which initialises `NSApplication`). Calling from
/// `ApplicationHandler::resumed` is the safe place.
#[cfg(target_os = "macos")]
pub fn set_app_icon_from_png_bytes(png_bytes: &[u8]) -> SetIconResult {
    use objc2::ClassType;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{MainThreadMarker, NSData, NSString};

    let Some(mtm) = MainThreadMarker::new() else {
        return SetIconResult::NotMainThread;
    };
    unsafe {
        let data = NSData::dataWithBytes_length(
            png_bytes.as_ptr() as *mut std::ffi::c_void,
            png_bytes.len(),
        );
        let alloc = NSImage::alloc();
        let Some(img) = NSImage::initWithData(alloc, &data) else {
            return SetIconResult::DecodeFailed;
        };
        // Drives dock + cmd-tab switcher.
        let app = NSApplication::sharedApplication(mtm);
        app.setApplicationIconImage(Some(&img));
        // Drives the standard About panel and any other consumer of
        // NSImage(named: "NSApplicationIcon"). Without this, About falls
        // back to whatever the bundle / Finder thinks the executable's
        // icon is, for an unbundled cargo-run binary, the generic
        // folder/exec glyph.
        let name = NSString::from_str("NSApplicationIcon");
        img.setName(Some(&name));
    }
    SetIconResult::Ok
}

#[cfg(not(target_os = "macos"))]
pub fn set_app_icon_from_png_bytes(_png_bytes: &[u8]) -> SetIconResult {
    SetIconResult::NotMacOs
}

/// AppUserModelID is a Windows-shell concept (taskbar grouping,
/// jump-list, pin-to-Start). No equivalent on macOS: the Dock
/// groups by bundle identifier, set elsewhere. Stub kept so the
/// `platform_shell` alias compiles symmetrically on both targets.
pub fn set_app_user_model_id(_id: &str) {}

/// `true` if the system is currently in Dark Mode. Reads
/// `NSApp.effectiveAppearance.name` and compares against
/// `NSAppearanceNameDarkAqua`, picking up both system Dark mode and
/// any per-app appearance override higher in the responder chain.
/// Must run on the main thread (NSApp constraint); off-thread
/// callers get `false`.
///
/// Returns `false` on non-macOS or if the lookup fails for any
/// reason: callers can layer their own override (e.g. an env var)
/// on top.
#[cfg(target_os = "macos")]
pub fn system_is_dark() -> bool {
    use objc2_app_kit::{NSAppearanceNameDarkAqua, NSApplication};
    use objc2_foundation::MainThreadMarker;

    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    unsafe {
        let app = NSApplication::sharedApplication(mtm);
        let appearance = app.effectiveAppearance();
        let name = appearance.name();
        // The reported name on Sonoma+ may be `NSAppearanceNameDarkAqua`,
        // `NSAppearanceNameAccessibilityHighContrastDarkAqua`, etc.
        // Treat any "Dark" appearance as dark.
        let n = name.to_string();
        n == NSAppearanceNameDarkAqua.to_string() || n.contains("Dark")
    }
}

#[cfg(not(target_os = "macos"))]
pub fn system_is_dark() -> bool {
    false
}

/// Force the app-wide native appearance to match the chosen theme,
/// independent of the system Light/Dark setting. Setting `NSApp`'s
/// appearance cascades to every window that doesn't override its own,
/// so native chrome, titlebars, the traffic-light area, menus, on
/// secondary windows (Viewer, Settings) stops rendering system-dark
/// under a light app theme (and vice versa). Main-thread only; a no-op
/// off the main thread or on non-macOS.
#[cfg(target_os = "macos")]
pub fn set_app_appearance(dark: bool) {
    use objc2_app_kit::{
        NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
    };
    use objc2_foundation::MainThreadMarker;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    unsafe {
        let app = NSApplication::sharedApplication(mtm);
        let name = if dark {
            NSAppearanceNameDarkAqua
        } else {
            NSAppearanceNameAqua
        };
        let appearance = NSAppearance::appearanceNamed(name);
        app.setAppearance(appearance.as_deref());
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_app_appearance(_dark: bool) {}

/// macOS "Show Desktop": the same wallpaper reveal the Dock performs
/// when you click the desktop or hit the Show Desktop shortcut. There
/// is no public API for it, so we resolve the Dock's private
/// `CoreDockSendNotification` symbol at runtime and post the
/// `com.apple.showdesktop.awake` toggle.
///
/// Two functions:
/// - [`show_desktop_available`]: `true` only when we resolved the
///   symbol on a new-enough macOS. UI affordances (toolbar button,
///   menu item) gate their visibility on this.
/// - [`show_desktop`]: performs the reveal, returning whether it
///   dispatched. Both are panic-free: on failure they report
///   unavailable / `false` rather than aborting, so a future OS change
///   that pulls the symbol degrades to "the button quietly disappears."
#[cfg(target_os = "macos")]
mod show_desktop_impl {
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::sync::OnceLock;

    // CoreDockSendNotification(CFStringRef notification, void *unused).
    // The Dock ignores the second argument; passing an extra ignored
    // pointer is safe under the C ABI on both x86_64 and arm64 (unused
    // argument registers are simply not read by the callee).
    type SendNotification = unsafe extern "C" fn(*const c_void, *const c_void);

    // libSystem (always linked on macOS) provides the dynamic loader.
    extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    const RTLD_LAZY: c_int = 0x1;

    // Frameworks that export `CoreDockSendNotification`, newest-OS first.
    // On macOS 26 (Tahoe) the old private `CoreDock.framework` is gone
    // from the dyld cache; the symbol is vended by the public
    // ApplicationServices umbrella (via its HIServices sub-framework),
    // which is always present. The legacy CoreDock path is kept as a
    // fallback for older systems where it still resolved directly.
    const CANDIDATE_PATHS: &[&str] = &[
        "/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices",
        "/System/Library/PrivateFrameworks/CoreDock.framework/CoreDock",
    ];

    // Floor we trust this private path on. The Show Desktop notification
    // long predates Big Sur, but gating here keeps us off any ancient
    // build where the symbol's contract might differ, and satisfies the
    // "right OS version" guard cheaply.
    const MIN_MAJOR: isize = 11;

    fn os_major() -> isize {
        use objc2_foundation::NSProcessInfo;
        // NSProcessInfo is thread-safe; no MainThreadMarker needed.
        let info = NSProcessInfo::processInfo();
        info.operatingSystemVersion().majorVersion
    }

    /// Resolved function-pointer address, or 0 when unavailable.
    /// Computed once: the `dlopen` happens on the first call only, and
    /// the handle is intentionally leaked for the process lifetime.
    fn resolved_addr() -> usize {
        static CELL: OnceLock<usize> = OnceLock::new();
        *CELL.get_or_init(|| {
            if os_major() < MIN_MAJOR {
                return 0;
            }
            let Ok(sym) = CString::new("CoreDockSendNotification") else {
                return 0;
            };
            // SAFETY: standard dlopen/dlsym against system frameworks.
            // We never dereference `handle`; dlsym returns null when the
            // symbol is absent, which we fold into the 0 sentinel and
            // move on to the next candidate framework.
            for path in CANDIDATE_PATHS {
                let Ok(path) = CString::new(*path) else {
                    continue;
                };
                unsafe {
                    let handle = dlopen(path.as_ptr(), RTLD_LAZY);
                    if handle.is_null() {
                        continue;
                    }
                    let addr = dlsym(handle, sym.as_ptr()) as usize;
                    if addr != 0 {
                        return addr;
                    }
                }
            }
            0
        })
    }

    pub(crate) fn available() -> bool {
        resolved_addr() != 0
    }

    pub(crate) fn trigger() -> bool {
        let addr = resolved_addr();
        if addr == 0 {
            return false;
        }
        // SAFETY: `addr` is a non-null CoreDockSendNotification pointer
        // (validated by resolved_addr). NSString is toll-free bridged to
        // CFStringRef, so its pointer is a valid first argument; the call
        // posts a Dock notification and returns void without retaining
        // the string past the call.
        unsafe {
            let func: SendNotification = std::mem::transmute(addr);
            let name = objc2_foundation::NSString::from_str("com.apple.showdesktop.awake");
            let name_ptr = (&*name as *const objc2_foundation::NSString) as *const c_void;
            func(name_ptr, std::ptr::null());
        }
        true
    }
}

/// `true` when the private Show Desktop path resolved on a supported
/// macOS. See [`show_desktop`]. Cheap after the first call (cached).
#[cfg(target_os = "macos")]
pub fn show_desktop_available() -> bool {
    show_desktop_impl::available()
}

/// Trigger the macOS Show Desktop reveal. Returns `true` if it
/// dispatched, `false` (never panics) if the symbol was unavailable.
#[cfg(target_os = "macos")]
pub fn show_desktop() -> bool {
    show_desktop_impl::trigger()
}

#[cfg(not(target_os = "macos"))]
pub fn show_desktop_available() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn show_desktop() -> bool {
    false
}

/// Bring every app window to the front, preserving their relative
/// z-order and leaving the key window unchanged: AppKit's
/// `-[NSApplication arrangeInFront:]`, the selector Finder's
/// Window ▸ Bring All to Front is wired to. Returns `false` (never
/// panics) off the main thread or on non-macOS so the caller can
/// fall back to raising windows one by one.
#[cfg(target_os = "macos")]
pub fn bring_all_windows_to_front() -> bool {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::MainThreadMarker;

    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    unsafe {
        NSApplication::sharedApplication(mtm).arrangeInFront(None);
    }
    true
}

#[cfg(not(target_os = "macos"))]
pub fn bring_all_windows_to_front() -> bool {
    false
}

/// Subscribe to macOS Appearance change notifications. The callback
/// fires on the main thread with the new dark-mode state every time
/// the user toggles System Settings → Appearance, or "Auto" mode
/// crosses the day/night boundary.
///
/// Thread contract: on macOS the callback fires on the MAIN thread,
/// but the shell-win32 twin of this function fires its callback on a
/// WORKER thread. Cross-platform callers must write the callback to
/// the weaker (win32) contract: thread-safe state only, no gpui
/// entities. `main.rs` does this via the atomic
/// `shell::set_system_theme_pending` cell that the Shell polls at
/// render.
///
/// Idempotent: re-registering replaces the callback without stacking
/// duplicate observers. Must be called on the main thread (after
/// winit has built its event loop / NSApp); off-thread or non-macOS
/// calls are a no-op.
#[cfg(target_os = "macos")]
pub fn start_system_theme_observer(callback: Box<dyn Fn(bool) + 'static>) {
    theme_observer::start(callback);
}

#[cfg(not(target_os = "macos"))]
pub fn start_system_theme_observer(_callback: Box<dyn Fn(bool) + 'static>) {}

/// Write the given paths to the general pasteboard as file URLs:
/// the cross-app file-copy verb (Finder pastes what we copy and vice
/// versa). Replaces the pasteboard's previous contents. Main-thread
/// only (AppKit). docs/features/FILE_OPS.md.
///
/// Each item carries its `is_dir` flag from the caller's cached
/// `FileEntry`: `fileURLWithPath_isDirectory:` exists precisely so
/// nobody has to stat here, and a stat per path on the main thread
/// would hang Cmd+C on a dead network mount.
#[cfg(target_os = "macos")]
pub fn clipboard_copy_file_urls(items: &[(&std::path::Path, bool)]) -> bool {
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardWriting};
    use objc2_foundation::{NSArray, NSString, NSURL};
    if objc2_foundation::MainThreadMarker::new().is_none() {
        return false;
    }
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let writers: Vec<objc2::rc::Retained<ProtocolObject<dyn NSPasteboardWriting>>> = items
            .iter()
            .filter_map(|(p, is_dir)| {
                let s = p.to_str()?;
                let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(s), *is_dir);
                Some(ProtocolObject::from_id(url))
            })
            .collect();
        if writers.is_empty() {
            return false;
        }
        let array: objc2::rc::Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>> =
            NSArray::from_vec(writers);
        let ok: bool = objc2::msg_send![&*pb, writeObjects: &*array];
        ok
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardFileOperation {
    Copy,
    Move,
}

pub fn clipboard_cut_file_urls(items: &[(&std::path::Path, bool)]) -> bool {
    clipboard_copy_file_urls(items)
}

#[cfg(not(target_os = "macos"))]
pub fn clipboard_copy_file_urls(_items: &[(&std::path::Path, bool)]) -> bool {
    false
}

/// Read file URLs off the general pasteboard (what Cmd+V pastes).
/// Empty when the pasteboard holds no file URLs. Main-thread only.
#[cfg(target_os = "macos")]
pub fn clipboard_read_file_urls() -> Vec<std::path::PathBuf> {
    use objc2::runtime::AnyObject;
    use objc2::ClassType as _;
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSArray, NSURL};
    if objc2_foundation::MainThreadMarker::new().is_none() {
        return Vec::new();
    }
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        let classes: objc2::rc::Retained<NSArray<AnyObject>> = {
            let url_cls: &AnyObject = std::mem::transmute(NSURL::class());
            objc2::msg_send_id![objc2::class!(NSArray), arrayWithObject: url_cls]
        };
        let read: Option<objc2::rc::Retained<NSArray<NSURL>>> = objc2::msg_send_id![&*pb, readObjectsForClasses: &*classes, options: std::ptr::null::<AnyObject>()];
        let Some(urls) = read else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..urls.count() {
            let url = urls.objectAtIndex(i);
            let is_file: bool = objc2::msg_send![&*url, isFileURL];
            if !is_file {
                continue;
            }
            if let Some(path) = url.path() {
                out.push(std::path::PathBuf::from(path.to_string()));
            }
        }
        out
    }
}

pub fn clipboard_read_file_urls_with_operation() -> (Vec<std::path::PathBuf>, ClipboardFileOperation)
{
    (clipboard_read_file_urls(), ClipboardFileOperation::Copy)
}

#[cfg(not(target_os = "macos"))]
pub fn clipboard_read_file_urls() -> Vec<std::path::PathBuf> {
    Vec::new()
}

/// Begin observing volume mount/unmount/rename via NSWorkspace's
/// notification center. The callback runs on the main thread after
/// every change; hosts re-list volumes and fan out. Main-thread-only,
/// idempotent (see [`start_system_theme_observer`] for the contract).
#[cfg(target_os = "macos")]
pub fn start_volume_observer(callback: Box<dyn Fn() + 'static>) {
    volume_observer::start(callback);
}

#[cfg(not(target_os = "macos"))]
pub fn start_volume_observer(_callback: Box<dyn Fn() + 'static>) {}

/// Begin observing system sleep/wake and display sleep/wake via
/// NSWorkspace's notification center. The callback runs on the main
/// thread with a [`PowerEvent`] for each transition; hosts pause
/// playback on sleep and refresh volume/directory state on wake.
/// Main-thread-only, idempotent (see [`start_system_theme_observer`]
/// for the contract).
///
/// Thread contract: the macOS callback fires on the MAIN thread, but
/// the shell-win32 twin fires on a WORKER thread: cross-platform
/// callers must write to the weaker (win32) contract (thread-safe
/// state / channel send only, no gpui entities).
#[cfg(target_os = "macos")]
pub fn start_power_observer(callback: Box<dyn Fn(ferail_core::power::PowerEvent) + 'static>) {
    power_observer::start(callback);
}

#[cfg(not(target_os = "macos"))]
pub fn start_power_observer(_callback: Box<dyn Fn(ferail_core::power::PowerEvent) + 'static>) {}

#[cfg(target_os = "macos")]
pub use power_assert::{prevent_idle_sleep, SleepBlocker};

/// Inert stand-in so cross-platform callers can hold a `SleepBlocker`
/// unconditionally on non-macOS workspace builds.
#[cfg(not(target_os = "macos"))]
pub struct SleepBlocker;

/// No-op on non-macOS workspace builds: returns an inert guard.
#[cfg(not(target_os = "macos"))]
pub fn prevent_idle_sleep(_reason: &str) -> Option<SleepBlocker> {
    None
}

/// Open a windowless native video player for `path` and start playback,
/// returning a handle (0 on failure). Frames are pulled out as BGRA
/// pixel buffers via [`video_overlay_copy_frame`] and drawn by the gpui
/// host as an image, there is no native overlay NSView. Main-thread
/// only. `on_ended` fires on the main thread when the video plays to
/// the end; it must defer (e.g. through a channel), not call player
/// APIs synchronously. See docs/features/VIEWER.md.
#[cfg(target_os = "macos")]
pub fn video_overlay_show(path: &std::path::Path, on_ended: Box<dyn Fn() + 'static>) -> u64 {
    video_overlay::show(path, on_ended)
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_show(_path: &std::path::Path, _on_ended: Box<dyn Fn() + 'static>) -> u64 {
    0
}

/// Pull the latest decoded frame as tightly-packed BGRA bytes plus its
/// `(width, height)` in pixels, or `None` when no new frame is ready
/// since the last pull (the caller keeps the previous frame / poster).
/// Main-thread only; stale ids give `None`.
#[cfg(target_os = "macos")]
pub fn video_overlay_copy_frame(id: u64) -> Option<(u32, u32, Vec<u8>)> {
    video_overlay::copy_frame(id)
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_copy_frame(_id: u64) -> Option<(u32, u32, Vec<u8>)> {
    None
}

/// Stop playback and remove a video overlay. Main-thread only; stale
/// ids no-op.
#[cfg(target_os = "macos")]
pub fn video_overlay_remove(id: u64) {
    video_overlay::remove(id);
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_remove(_id: u64) {}

/// Pause/resume a live video overlay. Main-thread only; stale ids no-op.
#[cfg(target_os = "macos")]
pub fn video_overlay_set_paused(id: u64, paused: bool) {
    video_overlay::set_paused(id, paused);
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_set_paused(_id: u64, _paused: bool) {}

/// Mute/unmute a live video overlay's audio. Main-thread only; stale ids no-op.
#[cfg(target_os = "macos")]
pub fn video_overlay_set_muted(id: u64, muted: bool) {
    video_overlay::set_muted(id, muted);
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_set_muted(_id: u64, _muted: bool) {}

/// Seek a live video overlay to the start and resume (loop). Main-thread
/// only; stale ids no-op.
#[cfg(target_os = "macos")]
pub fn video_overlay_restart(id: u64) {
    video_overlay::restart(id);
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_restart(_id: u64) {}

/// `(current, duration)` seconds of a live overlay's video; zeros when
/// unknown or for a stale id. Main-thread only.
#[cfg(target_os = "macos")]
pub fn video_overlay_time(id: u64) -> (f64, f64) {
    video_overlay::time(id)
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_time(_id: u64) -> (f64, f64) {
    (0.0, 0.0)
}

/// The current overlay video's intrinsic `(width, height)` in pixels;
/// `(0, 0)` while unknown or for a stale id. Main-thread only.
#[cfg(target_os = "macos")]
pub fn video_overlay_natural_size(id: u64) -> (f64, f64) {
    video_overlay::natural_size(id)
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_natural_size(_id: u64) -> (f64, f64) {
    (0.0, 0.0)
}

/// Seek a live overlay's video to `seconds`. Main-thread only; stale ids no-op.
#[cfg(target_os = "macos")]
pub fn video_overlay_seek(id: u64, seconds: f64) {
    video_overlay::seek(id, seconds);
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_seek(_id: u64, _seconds: f64) {}

/// Step a live overlay's video by `frames` frames (negative = backward).
/// Main-thread only; stale ids no-op.
#[cfg(target_os = "macos")]
pub fn video_overlay_step(id: u64, frames: i64) {
    video_overlay::step(id, frames);
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_step(_id: u64, _frames: i64) {}

/// Toggle a window's "stay on top" (floating) level from one of its
/// content NSViews. `floating` true raises it above normal windows;
/// false restores the normal level. Main-thread only; no-op otherwise.
#[cfg(target_os = "macos")]
pub fn set_window_floating(ns_view: *mut std::ffi::c_void, floating: bool) {
    use objc2::{msg_send, msg_send_id, rc::Retained, runtime::AnyObject};
    use objc2_app_kit::NSWindow;
    use objc2_foundation::MainThreadMarker;

    if MainThreadMarker::new().is_none() || ns_view.is_null() {
        return;
    }
    let view: &AnyObject = unsafe { &*(ns_view as *const AnyObject) };
    let window: Option<Retained<NSWindow>> = unsafe { msg_send_id![view, window] };
    if let Some(window) = window {
        // NSFloatingWindowLevel = 3, NSNormalWindowLevel = 0.
        let level: isize = if floating { 3 } else { 0 };
        unsafe {
            let _: () = msg_send![&*window, setLevel: level];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_window_floating(_ns_view: *mut std::ffi::c_void, _floating: bool) {}

/// Set the opacity of an entire top-level window, including its content and
/// chrome. `opacity` is clamped away from zero so the viewer can never become
/// completely invisible and impossible to recover. Main-thread only.
#[cfg(target_os = "macos")]
pub fn set_window_opacity(ns_view: *mut std::ffi::c_void, opacity: f32) {
    use objc2::{msg_send, msg_send_id, rc::Retained, runtime::AnyObject};
    use objc2_app_kit::NSWindow;
    use objc2_foundation::MainThreadMarker;

    if MainThreadMarker::new().is_none() || ns_view.is_null() {
        return;
    }
    let view: &AnyObject = unsafe { &*(ns_view as *const AnyObject) };
    let window: Option<Retained<NSWindow>> = unsafe { msg_send_id![view, window] };
    if let Some(window) = window {
        let alpha = opacity.clamp(0.2, 1.0) as f64;
        unsafe {
            let _: () = msg_send![&*window, setAlphaValue: alpha];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_window_opacity(_ns_view: *mut std::ffi::c_void, _opacity: f32) {}

/// Widen gpui's outbound drag-and-drop operation mask to Finder parity.
///
/// gpui's `NSDraggingSource` hardcodes `NSDragOperationCopy` for drag
/// sessions leaving the app (`gpui_macos/src/window.rs`,
/// `dragging_session_source_operation_mask`), so an external drop can only
/// ever copy: same-volume Finder drops don't move, and ⌥ / ⌘ / ⌃ change
/// nothing. Replacing the method on gpui's window classes with one that
/// offers Copy | Link | Generic | Move outside the app restores the
/// standard macOS contract: the destination picks the operation and
/// AppKit's built-in modifier filtering applies (⌥ forces copy and the
/// system draws the green “+” badge, ⌘ forces move, ⌃ makes an alias).
/// The within-application mask is left exactly as upstream had it
/// (Copy | Move); in-window drops are handled by our own `on_drop` code.
///
/// This is a runtime method replacement, not a fork: if upstream renames
/// `GPUIWindow` / `GPUIPanel` or grows a real API for this
/// (docs/GPUI-UPSTREAM.md #10 asks for allowed operations on
/// `ExternalDragPayload`), this quietly degrades to upstream's copy-only
/// behaviour. Call **after the first gpui window exists**: the classes
/// are registered lazily on first window construction. Idempotent;
/// returns whether at least one class was patched.
///
/// The same patching pass also wires the state
/// [`cancel_native_drag`] needs: an added
/// `draggingSession:willBeginAtPoint:` marks a session live, and a
/// wrapped `draggingSession:endedAtPoint:operation:` (chaining to gpui's
/// original) marks it over.
#[cfg(target_os = "macos")]
pub fn install_native_drag_operations() -> bool {
    use objc2::runtime::{AnyClass, AnyObject, Sel};

    let mask_sel = objc2::sel!(draggingSession:sourceOperationMaskForDraggingContext:);
    let begin_sel = objc2::sel!(draggingSession:willBeginAtPoint:);
    let ended_sel = objc2::sel!(draggingSession:endedAtPoint:operation:);
    let entered_sel = objc2::sel!(draggingEntered:);
    let exited_sel = objc2::sel!(draggingExited:);

    fn is_archive_promise_drag(dragging_info: *mut AnyObject) -> bool {
        // Ferail knows synchronously that it is starting an archive promise,
        // so the session flag is the primary signal for in-process handoff.
        // The pasteboard marker: declared by Ferail's promise-provider
        // subclass, and the type its windows register for so AppKit routes
        // the gesture here at all: is the defensive fallback for
        // callback-order variations.
        if native_drag::ARCHIVE_PROMISE_ACTIVE.load(std::sync::atomic::Ordering::SeqCst) {
            return true;
        }
        if dragging_info.is_null() {
            return false;
        }
        let pasteboard: *mut AnyObject =
            unsafe { objc2::msg_send![dragging_info, draggingPasteboard] };
        if pasteboard.is_null() {
            return false;
        }
        let marker =
            objc2_foundation::NSString::from_str(file_promise::ARCHIVE_PROMISE_PASTEBOARD_TYPE);
        let value: *mut AnyObject =
            unsafe { objc2::msg_send![pasteboard, stringForType: &*marker] };
        !value.is_null()
    }

    extern "C" fn dragging_entered(
        this: *mut AnyObject,
        sel: Sel,
        dragging_info: *mut AnyObject,
    ) -> usize {
        use std::sync::atomic::Ordering::SeqCst;

        // GPUI recognizes a file drag only through NSFilenamesPboardType and
        // answers None otherwise. A file promise has no filename path yet,
        // and AppKit only delivers this callback at all because `file_promise`
        // registered the window for Ferail's private marker type. Admit our
        // own promise sessions directly; GPUI's existing draggingUpdated /
        // performDragOperation callbacks then turn the gesture into
        // MouseMove/MouseUp for the retained ArchiveEntryDrag.
        if is_archive_promise_drag(dragging_info) {
            log::info!("archive promise drag: draggingEntered admitted on window {this:p}");
            return native_drag::OP_COPY;
        }

        let orig = native_drag::ORIG_ENTERED_IMP.load(SeqCst);
        if orig.is_null() {
            return native_drag::OP_NONE;
        }
        let orig: extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize =
            unsafe { std::mem::transmute(orig) };
        orig(this, sel, dragging_info)
    }

    extern "C" fn dragging_exited(this: *mut AnyObject, sel: Sel, dragging_info: *mut AnyObject) {
        // Ferail explicitly retires GPUI's typed drag when a file-promise
        // session starts and keeps archive coordinates in its own coordinator.
        // Always let GPUI process Exited so its ordinary hover/input state is
        // balanced for archive promises and every other drag alike.
        let orig = native_drag::ORIG_EXITED_IMP.load(std::sync::atomic::Ordering::SeqCst);
        if orig.is_null() {
            return;
        }
        let orig: extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) =
            unsafe { std::mem::transmute(orig) };
        orig(this, sel, dragging_info);
    }

    extern "C" fn source_operation_mask(
        _this: *mut AnyObject,
        _sel: Sel,
        _session: *mut AnyObject,
        context: isize,
    ) -> usize {
        if native_drag::CANCEL_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
            return native_drag::OP_NONE;
        }
        if context == native_drag::CONTEXT_WITHIN_APPLICATION {
            native_drag::OP_COPY | native_drag::OP_MOVE
        } else {
            native_drag::OP_COPY
                | native_drag::OP_LINK
                | native_drag::OP_GENERIC
                | native_drag::OP_MOVE
        }
    }

    extern "C" fn session_will_begin(
        _this: *mut AnyObject,
        _sel: Sel,
        _session: *mut AnyObject,
        _point: native_drag::CGPointRaw,
    ) {
        use std::sync::atomic::Ordering::SeqCst;
        native_drag::CANCEL_REQUESTED.store(false, SeqCst);
        native_drag::SESSION_ACTIVE.store(true, SeqCst);
    }

    extern "C" fn session_ended(
        this: *mut AnyObject,
        sel: Sel,
        session: *mut AnyObject,
        point: native_drag::CGPointRaw,
        operation: usize,
    ) {
        use std::sync::atomic::Ordering::SeqCst;
        native_drag::SESSION_ACTIVE.store(false, SeqCst);
        native_drag::ARCHIVE_PROMISE_ACTIVE.store(false, SeqCst);
        native_drag::CANCEL_REQUESTED.store(false, SeqCst);
        // Chain to gpui's implementation: it resets its own drag state
        // (synthetic-drag counter, FileDropEvent::Ended). Skipping it
        // would leave gpui believing a drag is still live.
        let orig = native_drag::ORIG_ENDED_IMP.load(SeqCst);
        if !orig.is_null() {
            let orig: extern "C" fn(
                *mut AnyObject,
                Sel,
                *mut AnyObject,
                native_drag::CGPointRaw,
                usize,
            ) = unsafe { std::mem::transmute(orig) };
            orig(this, sel, session, point, operation);
        }
    }

    let mut installed = false;
    for name in ["GPUIWindow", "GPUIPanel"] {
        let Some(class) = AnyClass::get(name) else {
            continue;
        };
        let class = class as *const AnyClass as *mut objc2::ffi::objc_class;
        unsafe {
            // NSUInteger (id self, SEL, id draggingInfo). Promise drags need
            // a pathless admission route before GPUI's legacy path parser.
            let imp: objc2::ffi::IMP = std::mem::transmute(
                dragging_entered as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize,
            );
            let prev =
                objc2::ffi::class_replaceMethod(class, entered_sel.as_ptr(), imp, c"Q@:@".as_ptr());
            let ours = dragging_entered
                as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize
                as *mut std::ffi::c_void;
            if let Some(prev) = prev {
                let prev = prev as *mut std::ffi::c_void;
                if prev != ours {
                    native_drag::ORIG_ENTERED_IMP.store(prev, std::sync::atomic::Ordering::SeqCst);
                }
            }

            // void (id self, SEL, id draggingInfo). Promise exits must not
            // make GPUI discard the retained in-process archive payload.
            let imp: objc2::ffi::IMP = std::mem::transmute(
                dragging_exited as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
            let prev =
                objc2::ffi::class_replaceMethod(class, exited_sel.as_ptr(), imp, c"v@:@".as_ptr());
            let ours = dragging_exited as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject)
                as *mut std::ffi::c_void;
            if let Some(prev) = prev {
                let prev = prev as *mut std::ffi::c_void;
                if prev != ours {
                    native_drag::ORIG_EXITED_IMP.store(prev, std::sync::atomic::Ordering::SeqCst);
                }
            }

            // NSUInteger (id self, SEL, id session, NSInteger ctx).
            let imp: objc2::ffi::IMP = std::mem::transmute(
                source_operation_mask
                    as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, isize) -> usize,
            );
            objc2::ffi::class_replaceMethod(class, mask_sel.as_ptr(), imp, c"Q@:@q".as_ptr());

            // void (id self, SEL, id session, NSPoint). gpui doesn't
            // implement willBegin, so this is an add, not a replace.
            let imp: objc2::ffi::IMP = std::mem::transmute(
                session_will_begin
                    as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, native_drag::CGPointRaw),
            );
            objc2::ffi::class_replaceMethod(
                class,
                begin_sel.as_ptr(),
                imp,
                c"v@:@{CGPoint=dd}".as_ptr(),
            );

            // void (id self, SEL, id session, NSPoint, NSDragOperation).
            // Both classes register the same upstream function, so one
            // stored original IMP serves both; guard against clobbering
            // it with our own wrapper on a second (idempotent) install.
            let imp: objc2::ffi::IMP = std::mem::transmute(
                session_ended
                    as extern "C" fn(
                        *mut AnyObject,
                        Sel,
                        *mut AnyObject,
                        native_drag::CGPointRaw,
                        usize,
                    ),
            );
            let prev = objc2::ffi::class_replaceMethod(
                class,
                ended_sel.as_ptr(),
                imp,
                c"v@:@{CGPoint=dd}Q".as_ptr(),
            );
            let ours = session_ended
                as extern "C" fn(
                    *mut AnyObject,
                    Sel,
                    *mut AnyObject,
                    native_drag::CGPointRaw,
                    usize,
                ) as *mut std::ffi::c_void;
            if let Some(prev) = prev {
                let prev = prev as *mut std::ffi::c_void;
                if prev != ours {
                    native_drag::ORIG_ENDED_IMP.store(prev, std::sync::atomic::Ordering::SeqCst);
                }
            }
        }
        installed = true;
    }
    installed
        && !native_drag::ORIG_ENTERED_IMP
            .load(std::sync::atomic::Ordering::SeqCst)
            .is_null()
        && !native_drag::ORIG_ENDED_IMP
            .load(std::sync::atomic::Ordering::SeqCst)
            .is_null()
        && !native_drag::ORIG_EXITED_IMP
            .load(std::sync::atomic::Ordering::SeqCst)
            .is_null()
}

#[cfg(not(target_os = "macos"))]
pub fn install_native_drag_operations() -> bool {
    false
}

/// Shared state + FFI for the native-drag override and its Esc-cancel.
#[cfg(target_os = "macos")]
mod native_drag {
    use std::sync::atomic::{AtomicBool, AtomicPtr};

    // NSDragOperation bits (AppKit).
    pub const OP_NONE: usize = 0;
    pub const OP_COPY: usize = 1;
    pub const OP_LINK: usize = 2;
    pub const OP_GENERIC: usize = 4;
    pub const OP_MOVE: usize = 16;
    // NSDraggingContext: 0 = outside the application, 1 = within it.
    pub const CONTEXT_WITHIN_APPLICATION: isize = 1;

    /// A native dragging session started from one of our windows is live.
    pub static SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);
    /// The current native session carries promised archive members. Raised by
    /// `file_promise::start` before `beginDraggingSession` so a destination's
    /// `draggingEntered:`: possibly delivered before that call returns: can
    /// admit the pathless gesture; lowered when the session ends.
    pub static ARCHIVE_PROMISE_ACTIVE: AtomicBool = AtomicBool::new(false);
    /// Esc was pressed mid-session: the mask collapses to None so no
    /// destination can accept the drop.
    pub static CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);
    /// gpui's original `draggingSession:endedAtPoint:operation:` IMP,
    /// chained from our wrapper.
    pub static ORIG_ENDED_IMP: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
    /// gpui's original `draggingEntered:` implementation, chained for every
    /// drag except Ferail's pathless archive promises.
    pub static ORIG_ENTERED_IMP: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
    /// gpui's original `draggingExited:` implementation, skipped only while
    /// an archive promise must retain its in-process payload across windows.
    pub static ORIG_EXITED_IMP: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

    /// `CGPoint` / `NSPoint` by value across the ObjC boundary.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGPointRaw {
        pub x: f64,
        pub y: f64,
    }

    // CGEvent: used to finish the physical gesture synthetically after a
    // cancel. Declared by hand, three functions don't justify a crate.
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        pub fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
        pub fn CGEventCreateMouseEvent(
            source: *const std::ffi::c_void,
            mouse_type: u32,
            position: CGPointRaw,
            button: u32,
        ) -> *mut std::ffi::c_void;
        pub fn CGEventGetLocation(event: *mut std::ffi::c_void) -> CGPointRaw;
        pub fn CGEventPost(tap: u32, event: *mut std::ffi::c_void);
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub fn CFRelease(cf: *const std::ffi::c_void);
    }

    pub const KCG_HID_EVENT_TAP: u32 = 0;
    pub const KCG_EVENT_LEFT_MOUSE_DRAGGED: u32 = 6;
    pub const KCG_EVENT_LEFT_MOUSE_UP: u32 = 2;
    pub const KCG_MOUSE_BUTTON_LEFT: u32 = 0;
}

/// Whether a native (outside-the-window) dragging session started from one
/// of our windows is currently live. gpui hands its in-window drag state to
/// the platform on promotion, so the host's `has_active_drag()` goes false
/// exactly when this goes true.
#[cfg(target_os = "macos")]
pub fn native_drag_session_active() -> bool {
    native_drag::SESSION_ACTIVE.load(std::sync::atomic::Ordering::SeqCst)
}

#[cfg(not(target_os = "macos"))]
pub fn native_drag_session_active() -> bool {
    false
}

/// Cancel the live native dragging session (Esc during a drag-out).
///
/// AppKit has no public "cancel this NSDraggingSession" call, so this uses
/// the one safe lever the source owns: collapse the source operation mask
/// to `None` (see `CANCEL_REQUESTED` in the swizzled mask method) so no
/// destination can accept the items, then finish the gesture synthetically:
/// a 1-px synthetic drag forces destinations to re-query the now-empty mask,
/// and a delayed synthetic mouse-up ends the session, which AppKit resolves
/// as a failed drag: the items animate back to their origin. Ordering
/// matters: the mouse-up is delayed (~80 ms, off-thread) so the None mask
/// propagates before the drop resolves; a same-instant up could still
/// resolve against the stale mask and perform the drop Esc meant to prevent.
/// The user's eventual physical button release delivers a stray mouse-up
/// with no drag in flight, which AppKit ignores.
///
/// No-op when no session is live.
#[cfg(target_os = "macos")]
pub fn cancel_native_drag() {
    use std::sync::atomic::Ordering::SeqCst;
    if !native_drag::SESSION_ACTIVE.load(SeqCst) {
        return;
    }
    native_drag::CANCEL_REQUESTED.store(true, SeqCst);
    // CGEventPost is documented thread-safe; do the delay off the UI thread.
    std::thread::spawn(|| unsafe {
        use native_drag::*;
        let probe = CGEventCreate(std::ptr::null());
        if probe.is_null() {
            return;
        }
        let at = CGEventGetLocation(probe);
        CFRelease(probe);

        let jiggle = CGEventCreateMouseEvent(
            std::ptr::null(),
            KCG_EVENT_LEFT_MOUSE_DRAGGED,
            CGPointRaw {
                x: at.x + 1.0,
                y: at.y,
            },
            KCG_MOUSE_BUTTON_LEFT,
        );
        if !jiggle.is_null() {
            CGEventPost(KCG_HID_EVENT_TAP, jiggle);
            CFRelease(jiggle);
        }
        std::thread::sleep(std::time::Duration::from_millis(80));
        // The session may have ended meanwhile (user released the button).
        if !SESSION_ACTIVE.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let up = CGEventCreateMouseEvent(
            std::ptr::null(),
            KCG_EVENT_LEFT_MOUSE_UP,
            at,
            KCG_MOUSE_BUTTON_LEFT,
        );
        if !up.is_null() {
            CGEventPost(KCG_HID_EVENT_TAP, up);
            CFRelease(up);
        }
    });
}

#[cfg(not(target_os = "macos"))]
pub fn cancel_native_drag() {}

// ---------------------------------------------------------------------------
// Window docking primitives (docs/features/DOCK.md).
//
// The host (`ferail-gpui`) drives the slide-in/out drawer entirely from its
// own GPUI tick: it polls the cursor, does the geometry, and moves the window.
// This crate only exposes the four AppKit calls that work has to bottom out in.
// All coordinates are macOS *global screen space*: origin at the bottom-left
// of the main display, y growing upward, which is the one space that
// `NSEvent.mouseLocation`, `NSScreen.visibleFrame`, and `NSWindow.frame` all
// already agree on, so the host's math needs no flipping.
// ---------------------------------------------------------------------------

/// Current global mouse location in macOS screen coordinates. A cheap class
/// method (no permission, thread-safe) the host polls on a timer only while a
/// dock edge is active. `(0.0, 0.0)` on non-macOS.
#[cfg(target_os = "macos")]
pub fn current_mouse_location() -> (f64, f64) {
    use objc2::{class, msg_send};
    use objc2_foundation::NSPoint;
    let p: NSPoint = unsafe { msg_send![class!(NSEvent), mouseLocation] };
    (p.x, p.y)
}

#[cfg(not(target_os = "macos"))]
pub fn current_mouse_location() -> (f64, f64) {
    (0.0, 0.0)
}

/// The `visibleFrame` (menu-bar/Dock-excluded) of the display the given
/// window currently occupies, as `(x, y, width, height)` in global screen
/// space. Falls back to the main screen when the window is off every display
/// (which happens once it is parked off-screen as a hidden drawer), so the
/// host can re-query safely. `None` off the main thread or with no screen.
#[cfg(target_os = "macos")]
pub fn screen_visible_frame_for_window(
    ns_view: *mut std::ffi::c_void,
) -> Option<(f64, f64, f64, f64)> {
    use objc2::{class, msg_send, msg_send_id, rc::Retained, runtime::AnyObject};
    use objc2_app_kit::{NSScreen, NSWindow};
    use objc2_foundation::{MainThreadMarker, NSRect};

    if MainThreadMarker::new().is_none() || ns_view.is_null() {
        return None;
    }
    let view: &AnyObject = unsafe { &*(ns_view as *const AnyObject) };
    let window: Option<Retained<NSWindow>> = unsafe { msg_send_id![view, window] };
    let window = window?;
    let screen: Option<Retained<NSScreen>> = unsafe { msg_send_id![&*window, screen] };
    let screen = match screen {
        Some(s) => s,
        None => {
            let main: Option<Retained<NSScreen>> =
                unsafe { msg_send_id![class!(NSScreen), mainScreen] };
            main?
        }
    };
    let frame: NSRect = unsafe { msg_send![&*screen, visibleFrame] };
    Some((
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn screen_visible_frame_for_window(
    _ns_view: *mut std::ffi::c_void,
) -> Option<(f64, f64, f64, f64)> {
    None
}

/// Move/resize a window (identified by one of its content NSViews) to the
/// given frame in global screen space. Deliberately *not* animated: the host
/// runs any slide itself, one step per GPUI frame, so this returns immediately
/// and never spins the run loop (Prime Directive). The host keeps the size
/// fixed during a slide, so this is a pure move and gpui never re-sizes its
/// drawable. Main-thread only; no-op otherwise.
#[cfg(target_os = "macos")]
pub fn set_window_frame(ns_view: *mut std::ffi::c_void, x: f64, y: f64, w: f64, h: f64) {
    use objc2::{
        msg_send, msg_send_id,
        rc::Retained,
        runtime::{AnyObject, Bool},
    };
    use objc2_app_kit::NSWindow;
    use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};

    if MainThreadMarker::new().is_none() || ns_view.is_null() {
        return;
    }
    let view: &AnyObject = unsafe { &*(ns_view as *const AnyObject) };
    let window: Option<Retained<NSWindow>> = unsafe { msg_send_id![view, window] };
    if let Some(window) = window {
        let frame = NSRect {
            origin: NSPoint { x, y },
            size: NSSize {
                width: w,
                height: h,
            },
        };
        unsafe {
            let _: () = msg_send![
                &*window,
                setFrame: frame,
                display: Bool::new(true),
                animate: Bool::new(false),
            ];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_window_frame(_ns_view: *mut std::ffi::c_void, _x: f64, _y: f64, _w: f64, _h: f64) {}

/// Toggle whether a window joins every Space and floats over full-screen apps
/// (`NSWindowCollectionBehaviorCanJoinAllSpaces | FullScreenAuxiliary`). A
/// docked drawer wants this so it stays reachable from any Space; pass `false`
/// to drop the behavior again on undock. Main-thread only; no-op otherwise.
///
/// Sets/clears ONLY those two bits, preserving whatever else the host
/// configured, writing `0` on undock used to clobber gpui's original
/// `collectionBehavior` (full-screen-primary participation, Stage
/// Manager behavior) for the rest of the session.
#[cfg(target_os = "macos")]
pub fn set_window_all_spaces(ns_view: *mut std::ffi::c_void, all_spaces: bool) {
    use objc2::{msg_send, msg_send_id, rc::Retained, runtime::AnyObject};
    use objc2_app_kit::NSWindow;
    use objc2_foundation::MainThreadMarker;

    if MainThreadMarker::new().is_none() || ns_view.is_null() {
        return;
    }
    let view: &AnyObject = unsafe { &*(ns_view as *const AnyObject) };
    let window: Option<Retained<NSWindow>> = unsafe { msg_send_id![view, window] };
    if let Some(window) = window {
        // CanJoinAllSpaces (1 << 0) | FullScreenAuxiliary (1 << 8).
        const BITS: usize = (1 << 0) | (1 << 8);
        unsafe {
            let current: usize = msg_send![&*window, collectionBehavior];
            let behavior = if all_spaces {
                current | BITS
            } else {
                current & !BITS
            };
            let _: () = msg_send![&*window, setCollectionBehavior: behavior];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_window_all_spaces(_ns_view: *mut std::ffi::c_void, _all_spaces: bool) {}

/// Whether the window is in native full screen (`styleMask` carries
/// `NSWindowStyleMaskFullScreen`). Docking must refuse a fullscreen
/// window: `setFrame:` on one confuses AppKit's Space bookkeeping.
/// `false` off the main thread / off macOS.
#[cfg(target_os = "macos")]
pub fn window_is_fullscreen(ns_view: *mut std::ffi::c_void) -> bool {
    use objc2::{msg_send, msg_send_id, rc::Retained, runtime::AnyObject};
    use objc2_app_kit::NSWindow;
    use objc2_foundation::MainThreadMarker;

    if MainThreadMarker::new().is_none() || ns_view.is_null() {
        return false;
    }
    let view: &AnyObject = unsafe { &*(ns_view as *const AnyObject) };
    let window: Option<Retained<NSWindow>> = unsafe { msg_send_id![view, window] };
    match window {
        Some(window) => {
            let mask: usize = unsafe { msg_send![&*window, styleMask] };
            // NSWindowStyleMaskFullScreen = 1 << 14.
            mask & (1 << 14) != 0
        }
        None => false,
    }
}

#[cfg(not(target_os = "macos"))]
pub fn window_is_fullscreen(_ns_view: *mut std::ffi::c_void) -> bool {
    false
}

/// A window's current frame as `(x, y, width, height)` in global screen
/// space, so the host can remember where to put it back when undocking.
/// `None` off the main thread or with no window. Main-thread only.
#[cfg(target_os = "macos")]
pub fn window_frame(ns_view: *mut std::ffi::c_void) -> Option<(f64, f64, f64, f64)> {
    use objc2::{msg_send, msg_send_id, rc::Retained, runtime::AnyObject};
    use objc2_app_kit::NSWindow;
    use objc2_foundation::{MainThreadMarker, NSRect};

    if MainThreadMarker::new().is_none() || ns_view.is_null() {
        return None;
    }
    let view: &AnyObject = unsafe { &*(ns_view as *const AnyObject) };
    let window: Option<Retained<NSWindow>> = unsafe { msg_send_id![view, window] };
    let window = window?;
    let frame: NSRect = unsafe { msg_send![&*window, frame] };
    Some((
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn window_frame(_ns_view: *mut std::ffi::c_void) -> Option<(f64, f64, f64, f64)> {
    None
}

// ---------------------------------------------------------------------------
// Privilege escalation + locked-file diagnostics (resilient file operations).
//
// Same public surface in every shell crate (`platform_shell::*`). The host
// gpui builds the op descriptor; this crate only knows how to "re-launch the
// current binary elevated and wait": it never sees the op type.
// ---------------------------------------------------------------------------

/// A process holding a file the user is trying to mutate, so a "the file is
/// open in X" message and a force-close affordance can name it. Identical
/// shape in every shell crate so it round-trips through the `platform_shell`
/// alias.
#[derive(Clone, Debug)]
pub struct LockingProcess {
    pub pid: u32,
    pub name: String,
}

/// Whether this platform can run a privileged retry. macOS: yes, via osascript
/// `with administrator privileges`.
#[cfg(target_os = "macos")]
pub fn elevation_available() -> bool {
    true
}
#[cfg(not(target_os = "macos"))]
pub fn elevation_available() -> bool {
    false
}

/// Re-launch THIS executable with `args`, elevated, and block until it exits;
/// returns the child exit code. macOS routes through osascript `do shell script
/// … with administrator privileges` (one OS auth prompt; the child runs as
/// root). The caller passes `--elevated-op <descriptor>` so the same binary
/// performs the file op as root and writes a result file. Blocks on the auth
/// dialog: call off the UI thread.
#[cfg(target_os = "macos")]
pub fn run_elevated_self(args: &[String]) -> Result<i32, String> {
    elevation::run_elevated_self(args)
}
#[cfg(not(target_os = "macos"))]
pub fn run_elevated_self(_args: &[String]) -> Result<i32, String> {
    Err("elevation is not available on this platform".into())
}

/// Whether this platform can enumerate the processes holding a locked file.
/// macOS file locking is advisory and rarely the cause, so this is deferred:
/// it lands with the Windows Restart Manager work.
pub fn lock_diagnostics_available() -> bool {
    false
}

/// Processes currently holding `path` open. Empty when unknown/unsupported.
pub fn processes_using(_path: &std::path::Path) -> Vec<LockingProcess> {
    Vec::new()
}

/// Result of a capped lock scan over one or more roots. Identical shape in
/// every shell crate; only Windows fills it (Restart Manager).
#[derive(Clone, Debug, Default)]
pub struct LockScan {
    pub holders: Vec<LockingProcess>,
    /// Files actually checked.
    pub scanned: usize,
    /// The file cap cut the walk short.
    pub truncated: bool,
}

/// Processes holding open any file under `roots`. Unsupported on macOS:
/// lands with an lsof-backed `processes_using`.
pub fn processes_using_tree(_roots: &[std::path::PathBuf], _max_files: usize) -> LockScan {
    LockScan::default()
}

/// Ask the given processes to close so a locked file can be retried.
/// Unsupported on macOS for now.
pub fn force_close_processes(_pids: &[u32]) -> Result<(), String> {
    Err("closing the locking process isn't supported on this platform yet".into())
}

// ---------------------------------------------------------------------------
// Path-backed platform roots (WIN-017).
//
// WSL is Windows-only. These mirrors keep the GPUI capability surface
// platform-neutral; macOS simply publishes no roots.
// ---------------------------------------------------------------------------

pub fn discover_path_backed_platform_roots(
    _cancel: &std::sync::atomic::AtomicBool,
) -> Vec<ferail_core::platform_locations::PathBackedPlatformRoot> {
    Vec::new()
}

pub fn activate_path_backed_platform_root(
    _id: &ferail_core::platform_locations::PlatformRootId,
    _cancel: &std::sync::atomic::AtomicBool,
) -> Result<std::path::PathBuf, ferail_core::platform_locations::PlatformRootErrorKind> {
    Err(ferail_core::platform_locations::PlatformRootErrorKind::Unavailable)
}

pub fn is_wsl_path(_path: &std::path::Path) -> bool {
    false
}

pub fn resolve_wsl_symlink_path(
    _path: &std::path::Path,
    _cancel: &std::sync::atomic::AtomicBool,
) -> Result<std::path::PathBuf, ferail_core::platform_locations::PlatformRootErrorKind> {
    Err(ferail_core::platform_locations::PlatformRootErrorKind::Unavailable)
}

/// osascript-backed privileged re-exec. Two layers of quoting: each token is
/// POSIX single-quoted for the shell, and the whole command is escaped as an
/// AppleScript string literal.
#[cfg(target_os = "macos")]
mod elevation {
    pub fn run_elevated_self(args: &[String]) -> Result<i32, String> {
        let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
        let mut shell_cmd = shell_quote(&exe.to_string_lossy());
        for a in args {
            shell_cmd.push(' ');
            shell_cmd.push_str(&shell_quote(a));
        }
        let script = format!(
            "do shell script {} with administrator privileges",
            applescript_quote(&shell_cmd)
        );
        let out = std::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(&script)
            .output()
            .map_err(|e| format!("osascript: {e}"))?;
        if out.status.success() {
            // `do shell script` raises on a non-zero child exit, so reaching
            // here means the worker ran and wrote its result file.
            return Ok(0);
        }
        let err = String::from_utf8_lossy(&out.stderr);
        // -128 / "User canceled" == the user dismissed the auth dialog.
        if err.contains("-128") || err.contains("User canceled") {
            Err("cancelled".into())
        } else {
            Err(format!("elevation failed: {}", err.trim()))
        }
    }

    /// Wrap in single quotes, turning each interior `'` into `'\''`.
    pub(crate) fn shell_quote(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('\'');
        for ch in s.chars() {
            if ch == '\'' {
                out.push_str("'\\''");
            } else {
                out.push(ch);
            }
        }
        out.push('\'');
        out
    }

    /// AppleScript string literal: wrap in `"…"`, escaping `\` and `"`.
    pub(crate) fn applescript_quote(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for ch in s.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                _ => out.push(ch),
            }
        }
        out.push('"');
        out
    }
}

#[cfg(all(test, target_os = "macos"))]
mod terminal_tests {
    use ferail_core::terminal::{TerminalMode, TerminalSpec};

    /// A fake CLI "terminal": a script that records its argv and cwd,
    /// so the custom-program launch path (arg resolution, `{dir}`
    /// expansion, working-directory fallback) is verified end-to-end
    /// without opening a window.
    fn recorder_script(dir: &std::path::Path, out: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("fake-term.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n{{ pwd; printf '%s\\n' \"$@\"; }} > '{}'\n",
                out.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn wait_for(out: &std::path::Path) -> String {
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(out) {
                if !s.is_empty() {
                    return s;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("recorder output never appeared at {}", out.display());
    }

    #[test]
    fn custom_binary_gets_resolved_args() {
        let tmp = std::env::temp_dir().join(format!("ferail-term-test-{}", std::process::id()));
        let target = tmp.join("target dir");
        std::fs::create_dir_all(&target).unwrap();
        let out = tmp.join("argv.txt");
        let script = recorder_script(&tmp, &out);

        let spec = TerminalSpec {
            program: Some(script.to_string_lossy().into_owned()),
            args: vec!["--cwd".into(), "{dir}".into()],
            mode: TerminalMode::Standard,
        };
        super::open_terminal_with(&target, &spec);

        let recorded = wait_for(&out);
        let lines: Vec<&str> = recorded.lines().collect();
        // `{dir}` appeared in the args, so cwd stays wherever the app was.
        assert_eq!(lines[1..], ["--cwd", &*target.to_string_lossy()]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn custom_binary_without_dir_placeholder_inherits_cwd() {
        let tmp = std::env::temp_dir().join(format!("ferail-term-cwd-{}", std::process::id()));
        let target = tmp.join("workdir");
        std::fs::create_dir_all(&target).unwrap();
        let out = tmp.join("argv.txt");
        let script = recorder_script(&tmp, &out);

        let spec = TerminalSpec {
            program: Some(script.to_string_lossy().into_owned()),
            args: vec![],
            mode: TerminalMode::Standard,
        };
        super::open_terminal_with(&target, &spec);

        let recorded = wait_for(&out);
        // No `{dir}` in the args → the child runs *in* the folder.
        // `pwd` may come back through /private on macOS; canonicalize both.
        let reported = std::fs::canonicalize(recorded.lines().next().unwrap()).unwrap();
        assert_eq!(reported, std::fs::canonicalize(&target).unwrap());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
