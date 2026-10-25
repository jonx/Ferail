// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper

//! AROS platform shell.
//!
//! The AROS arm of the `platform_shell` indirection (see
//! [`ferail_gpui::platform_shell`]): the same `pub fn` and type surface as
//! `ferail-shell-mac`, `ferail-shell-win32` and `ferail-shell-linux`, so the
//! alias round-trips, with AROS behind it. Where AROS has the facility, the
//! function uses it; where it has none, the function says so through its
//! result (`Err`, `None`, `false`, an empty list) instead of reporting a
//! success that did not happen. Nothing here falls back to another
//! platform's conventions.
//!
//! The AROS facilities used, through the C glue in `c/shell_glue.c`:
//!
//! | Need | AROS facility |
//! |---|---|
//! | open a file with its application, reveal a path | `workbench.library` `OpenWorkbenchObject` (the default tool, or the drawer window) |
//! | open a URL | `openurl.library` (the configured browser or mailer) |
//! | open a shell in a directory | DOS `SystemTags("NewShell")` in that directory |
//! | choose a folder | `asl.library` drawer requester |
//! | copy/cut files | `clipboard.device` unit 0, FTXT/CHRS plus typed Ferail FSAF references |
//! | copy text | `clipboard.device` unit 0, IFF FTXT through `iffparse.library` |
//! | duplicate, link, zip | DOS files through `std::fs`; soft links through `symlink` |
//!
//! Unavailable on AROS and reported as such: Open With candidates (no
//! application registry), thumbnails and type icons (the app draws its
//! glyphs; `icon.library` images are a later step), Finder
//! tags, Quick Look, idle-sleep inhibition, system theme observation, volume
//! and power observers, native video overlay, elevation and lock diagnostics,
//! window docking, Windows and WSL roots.
//!
//! Blocking calls (Workbench, the ASL requester, DOS) run on whichever
//! thread the caller uses; `ferail-gpui` dispatches them from workers as it
//! does for the other shells.

#[cfg(target_os = "aros")]
use std::ffi::c_int;
use std::ffi::{CString, c_void};
use std::path::{Path, PathBuf};

use ferail_core::commands::TagColor;
use ferail_core::power::PowerEvent;

// =============================================================
// Shared types (declared with identical shape in every shell crate
// so they round-trip through the `platform_shell` alias).
// =============================================================

/// An application an "Open With" menu would offer for a path. AROS has no
/// application registry to enumerate: the list stays empty.
#[derive(Debug, Clone)]
pub struct OpenWithCandidate {
    pub name: String,
    pub path: PathBuf,
    pub is_default: bool,
}

/// Result of [`set_app_icon_from_png_bytes`], mirrored from the other shells.
/// `NotMacOs` is the variant name every non-macOS shell returns.
#[derive(Debug)]
pub enum SetIconResult {
    Ok,
    NotMacOs,
    NotMainThread,
    DecodeFailed,
}

/// Guard shape shared with the other shells. Never constructed on AROS:
/// [`prevent_idle_sleep`] has no facility to hold.
pub struct SleepBlocker;

// =============================================================
// The C glue (c/shell_glue.c)
// =============================================================

#[cfg(target_os = "aros")]
mod glue {
    use std::ffi::{c_char, c_int};
    unsafe extern "C" {
        pub fn fsa_open_wb_object(path: *const c_char) -> c_int;
        pub fn fsa_open_url(url: *const c_char) -> c_int;
        pub fn fsa_run_in_dir(command: *const c_char, dir: *const c_char) -> c_int;
        pub fn fsa_pick_drawer(title: *const c_char, initial: *const c_char) -> *mut c_char;
        pub fn fsa_free_string(s: *mut c_char);
        pub fn fsa_clip_write_text(text: *const u8, len: c_int) -> c_int;
    }
}

/// The path as the DOS side wants it: raw bytes, NUL-terminated. AROS paths
/// are byte strings; a `PathBuf::join` of a device root leaves a `DEV:/x`
/// spelling that DOS reads as the parent of the root, so the slash after
/// the colon is dropped.
#[cfg_attr(not(target_os = "aros"), allow(dead_code))]
fn dos_path(path: &Path) -> std::io::Result<CString> {
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;
    #[cfg(not(unix))]
    let owned = path.to_string_lossy().into_owned();
    #[cfg(not(unix))]
    let path: &Path = Path::new(owned.as_str());
    let bytes = path.as_os_str().as_bytes();
    let normalized: Vec<u8> = match bytes.iter().position(|&b| b == b':') {
        Some(i) if bytes.get(i + 1) == Some(&b'/') => {
            let mut v = bytes[..=i].to_vec();
            v.extend_from_slice(&bytes[i + 2..]);
            v
        }
        _ => bytes.to_vec(),
    };
    CString::new(normalized)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn unavailable(what: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("{what} is not available on AROS"),
    )
}

#[cfg(target_os = "aros")]
fn wb_open(path: &Path) -> std::io::Result<()> {
    let c = dos_path(path)?;
    // SAFETY: a NUL-terminated string; the glue copies nothing past the call.
    match unsafe { glue::fsa_open_wb_object(c.as_ptr()) } {
        0 => Ok(()),
        _ => Err(std::io::Error::other(format!(
            "Workbench could not open {}",
            path.display()
        ))),
    }
}

// =============================================================
// App lifecycle / chrome / about
// =============================================================

/// The About text lives in the app's own About dialog; the Intuition menu
/// has no system About panel to configure.
pub fn set_about_options(_app_name: &str, _tagline: &str, _version: &str, _copyright: &str) {}

/// No system About panel: the app opens its own dialog instead.
pub fn show_about_panel() {}

/// Wanderer has no "show desktop" gesture: `false` hides the menu item.
pub fn show_desktop_available() -> bool {
    false
}

pub fn show_desktop() -> bool {
    false
}

/// Intuition has no "raise all of this application's windows"; the caller
/// raises each window through GPUI.
pub fn bring_all_windows_to_front() -> bool {
    false
}

// =============================================================
// Pickers / launching
// =============================================================

/// Choose a drawer through an ASL requester. `None` when cancelled.
#[cfg(target_os = "aros")]
pub fn pick_folder() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let title = CString::new("Choose a drawer").ok()?;
    // SAFETY: the glue returns an AllocVec'd copy or null; freed below.
    let raw = unsafe { glue::fsa_pick_drawer(title.as_ptr(), std::ptr::null()) };
    if raw.is_null() {
        return None;
    }
    let bytes = unsafe { std::ffi::CStr::from_ptr(raw) }.to_bytes().to_vec();
    unsafe { glue::fsa_free_string(raw) };
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}
#[cfg(not(target_os = "aros"))]
pub fn pick_folder() -> Option<PathBuf> {
    None
}

/// Hand a URL to openurl.library, which runs the configured browser or
/// mailer. Failure is logged by the caller's absence of effect: the shell
/// surface gives this function no result channel.
#[cfg(target_os = "aros")]
pub fn open_url(url: &str) {
    if let Ok(c) = CString::new(url) {
        // SAFETY: a NUL-terminated string.
        let _ = unsafe { glue::fsa_open_url(c.as_ptr()) };
    }
}
#[cfg(not(target_os = "aros"))]
pub fn open_url(_url: &str) {}

/// Open a file or drawer as Wanderer would: a project through its default
/// tool, a tool by running it, a drawer as a window.
#[cfg(target_os = "aros")]
pub fn open_with_default(path: &Path) -> std::io::Result<()> {
    wb_open(path)
}
#[cfg(not(target_os = "aros"))]
pub fn open_with_default(_path: &Path) -> std::io::Result<()> {
    Err(unavailable("Workbench"))
}

/// Open a text file in its default tool (the icon's, or the DefIcons text
/// tool), which is what Workbench does with a project.
pub fn edit_text_file(path: &Path) -> std::io::Result<()> {
    open_with_default(path)
}

/// Reveal `path` by opening its drawer as a Wanderer window; Wanderer has
/// no "select this icon" call, so the window is the reveal.
pub fn reveal_in_finder(path: &Path) {
    let _ = try_reveal_in_finder(path);
}

pub fn try_reveal_in_finder(path: &Path) -> std::io::Result<()> {
    let drawer = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(path);
    open_with_default(drawer)
}

/// Open a shell in `path` with the default spec. See [`open_terminal_with`].
pub fn open_terminal(path: &Path) {
    open_terminal_with(path, &ferail_core::terminal::TerminalSpec::default());
}

/// Open a shell in `path` (a directory): the DOS `NewShell` command, run as
/// its own process with that directory current, or the user's custom
/// program with its parameters when the spec names one. There is no
/// administrator mode on AROS; the spec's admin flag changes nothing.
#[cfg(target_os = "aros")]
pub fn open_terminal_with(path: &Path, spec: &ferail_core::terminal::TerminalSpec) {
    let dir = path.to_string_lossy();
    let command = match spec.program() {
        Some(program) => {
            let (args, _) = spec.resolved_args(&dir);
            let mut line = program.to_string();
            for arg in args {
                line.push(' ');
                line.push_str(&arg);
            }
            line
        }
        None => "NewShell".to_string(),
    };
    let (Ok(c_command), Ok(c_dir)) = (CString::new(command), dos_path(path)) else {
        return;
    };
    // SAFETY: NUL-terminated strings; the glue duplicates the directory lock
    // into the new process and releases its own.
    let _ = unsafe { glue::fsa_run_in_dir(c_command.as_ptr(), c_dir.as_ptr()) };
}
#[cfg(not(target_os = "aros"))]
pub fn open_terminal_with(_path: &Path, _spec: &ferail_core::terminal::TerminalSpec) {}

/// AROS keeps no registry of applications per file type to enumerate: the
/// default tool is the only association, and [`open_with_default`] uses it.
pub fn open_with_candidates(_path: &Path) -> Vec<OpenWithCandidate> {
    Vec::new()
}

/// Run `app_path` as a tool with `target` as its argument, as Workbench does
/// for a tool started with a project.
#[cfg(target_os = "aros")]
pub fn open_with_app(target: &Path, app_path: &Path) -> Result<(), String> {
    let app = dos_path(app_path).map_err(|e| e.to_string())?;
    let file = dos_path(target).map_err(|e| e.to_string())?;
    let mut line = app.into_bytes();
    line.extend_from_slice(b" \"");
    line.extend_from_slice(file.as_bytes());
    line.push(b'"');
    let c = CString::new(line).map_err(|e| e.to_string())?;
    let dir = target
        .parent()
        .map(dos_path)
        .transpose()
        .map_err(|e| e.to_string())?;
    // SAFETY: NUL-terminated strings.
    let rc = unsafe {
        glue::fsa_run_in_dir(
            c.as_ptr(),
            dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("could not run {}", app_path.display()))
    }
}
#[cfg(not(target_os = "aros"))]
pub fn open_with_app(_target: &Path, _app_path: &Path) -> Result<(), String> {
    Err("open_with_app is not available on AROS".into())
}

/// Open every `target` with the tool at `app_path`, one process each.
pub fn open_with_app_many(targets: &[PathBuf], app_path: &Path) -> Result<(), String> {
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

/// Place plain text on clipboard unit 0 as IFF FTXT. The clipboard
/// convention is ISO-8859-1: characters outside it are written as `?`.
#[cfg(target_os = "aros")]
pub fn copy_to_clipboard(text: &str) {
    let latin1: Vec<u8> = text
        .chars()
        .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
        .collect();
    // SAFETY: the glue reads `len` bytes of `latin1` during the call only.
    let _ = unsafe { glue::fsa_clip_write_text(latin1.as_ptr(), latin1.len() as c_int) };
}
#[cfg(not(target_os = "aros"))]
pub fn copy_to_clipboard(_text: &str) {}

/// Native file clipboard metadata operation, carried with the raw DOS paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardFileOperation {
    Copy,
    Move,
}

#[cfg(any(target_os = "aros", all(test, unix)))]
mod file_clipboard;
#[cfg(target_os = "aros")]
pub use file_clipboard::{
    Request as FileClipboardRequest, Session as FileClipboardSession,
    accept_result as accept_file_clipboard_result, reserve as reserve_file_clipboard,
    shutdown as shutdown_file_clipboard,
};

/// Synchronous compatibility entry points; AROS callers must use workers.
/// UI handlers reserve an ordered request and run its I/O on an executor.
pub fn clipboard_copy_file_urls(items: &[(&Path, bool)]) -> bool {
    clipboard_write_file_urls(items, ClipboardFileOperation::Copy)
}
pub fn clipboard_cut_file_urls(items: &[(&Path, bool)]) -> bool {
    clipboard_write_file_urls(items, ClipboardFileOperation::Move)
}
fn clipboard_write_file_urls(items: &[(&Path, bool)], operation: ClipboardFileOperation) -> bool {
    #[cfg(target_os = "aros")]
    {
        let owned = items
            .iter()
            .map(|(p, d)| (p.to_path_buf(), *d))
            .collect::<Vec<_>>();
        return file_clipboard::reserve()
            .and_then(|job| job.write(&owned, operation))
            .is_ok();
    }
    #[cfg(not(target_os = "aros"))]
    {
        let _ = (items, operation);
        false
    }
}
pub fn clipboard_read_file_urls() -> Vec<PathBuf> {
    clipboard_read_file_urls_with_operation().0
}
pub fn clipboard_read_file_urls_with_operation() -> (Vec<PathBuf>, ClipboardFileOperation) {
    #[cfg(target_os = "aros")]
    {
        return file_clipboard::reserve()
            .and_then(|job| job.read())
            .unwrap_or((Vec::new(), ClipboardFileOperation::Copy));
    }
    #[cfg(not(target_os = "aros"))]
    (Vec::new(), ClipboardFileOperation::Copy)
}

// =============================================================
// File operations
// =============================================================

/// Duplicate a path in place: `stem (copy).ext`, then `stem (copy N).ext`.
/// Directories are copied recursively; links inside are skipped, never
/// followed.
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

/// AROS volumes are not ejected from a file manager: removable media is
/// released by the handler when the volume is no longer in use.
pub fn eject_volume(_path: &Path) -> Result<(), String> {
    Err("ejecting a volume is not available on AROS".into())
}

pub fn eject_device(_volume_paths: &[&Path]) -> Result<(), String> {
    Err("ejecting a device is not available on AROS".into())
}

/// No per-process open-file table to scan on AROS.
pub fn volume_busy_processes(_path: &Path) -> Vec<ferail_core::BusyApp> {
    Vec::new()
}

pub fn activate_app(_pid: i32) -> bool {
    false
}

/// A DOS soft link named `stem (link).ext` beside the target.
pub fn make_alias(target: &Path) -> Result<PathBuf, String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("no parent dir for {}", target.display()))?;
    make_alias_in(target, parent)
}

/// A DOS soft link to `target` inside `dest_dir`, named `stem (link).ext`
/// with a `(link N)` suffix if taken (capped at 99). The link holds the
/// target's absolute path so it resolves from anywhere.
#[cfg(target_os = "aros")]
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
        if candidate.symlink_metadata().is_ok() {
            continue;
        }
        std::os::unix::fs::symlink(&link_target, &candidate).map_err(|e| e.to_string())?;
        return Ok(candidate);
    }
    Err("make_alias_in: exhausted ' (link 1..99)' slots".into())
}
#[cfg(not(target_os = "aros"))]
pub fn make_alias_in(_target: &Path, _dest_dir: &Path) -> Result<PathBuf, String> {
    Err("make_alias_in is not available on AROS".into())
}

/// Compress `targets` into a `.zip` next to the first target. Single source
/// → `<filename>.zip`; multiple → `Archive.zip`; next free `(N)` suffix if
/// taken (capped at 99). Directories are walked recursively; links are
/// skipped. Platform-neutral: identical to the other shells. Call on a
/// worker thread; large archives take seconds.
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

/// Recursive directory copy for [`duplicate_path`]; links are skipped.
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

// =============================================================
// Previews / thumbnails
// =============================================================

/// No system preview on AROS; the in-app viewer is the preview.
pub fn show_quick_look(_paths: &[&Path]) -> Result<(), String> {
    Err("a system preview is not available on AROS".into())
}

/// No content thumbnail provider on AROS yet; the app falls back to its
/// type glyphs. Datatypes are the candidate provider.
pub fn fetch_quick_look_thumbnail(_path: &Path, _size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    None
}

pub fn fetch_preview_image(_path: &Path, _size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    None
}

/// Icon images (`icon.library`, DefIcons) are a later step.
pub fn fetch_type_icon(_path: &Path, _size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    None
}

// =============================================================
// Get Info: shell facts
// =============================================================

/// Shell-sourced Get Info facts, same shape as the other shells. AROS has no
/// per-file type identifier, "date added" or hide-extension flag.
#[derive(Clone, Debug, Default)]
pub struct ShellInfo {
    pub uti: Option<String>,
    pub kind: Option<String>,
    pub added_unix: Option<i64>,
    pub hidden_extension: Option<bool>,
    pub is_package: Option<bool>,
    pub is_alias: Option<bool>,
}

pub fn read_shell_info(_path: &Path) -> ShellInfo {
    ShellInfo::default()
}

pub fn set_hidden_extension(_path: &Path, _hide: bool) -> Result<(), String> {
    Err("hiding the extension per file is not available on AROS".into())
}

// =============================================================
// Tags, updates
// =============================================================

/// No desktop-wide tag store on AROS.
pub const SUPPORTS_TAGS: bool = false;

pub fn prefer_installer_updates() -> bool {
    false
}

pub fn launch_update_installer(_path: &Path) -> std::io::Result<()> {
    Err(unavailable("the update installer"))
}

pub fn read_canonical_tags(_path: &Path) -> Vec<TagColor> {
    Vec::new()
}

pub fn toggle_tag(_path: &Path, _color: TagColor) -> Result<(), String> {
    Err("tags are not available on AROS".into())
}

// =============================================================
// Appearance / theme
// =============================================================

/// AROS has no system light/dark preference; the app's own theme setting
/// decides.
pub fn system_is_dark() -> bool {
    false
}

pub fn set_app_appearance(_dark: bool) {}

/// No system theme change to observe: the callback is dropped.
pub fn start_system_theme_observer(_callback: Box<dyn Fn(bool) + 'static>) {}

// =============================================================
// App identity / icon
// =============================================================

/// The running program's icon is its `.info` file; there is no runtime swap.
pub fn set_app_icon_from_png_bytes(_png_bytes: &[u8]) -> SetIconResult {
    SetIconResult::NotMacOs
}

pub fn set_app_user_model_id(_id: &str) {}

/// No bundle concept on AROS.
pub fn app_bundle_path() -> Option<String> {
    None
}

// =============================================================
// Volumes / power
// =============================================================

/// Volume mount and unmount notifications (DOS notify, the volume list) are
/// a later step; the callback is dropped and the app rescans on demand.
pub fn start_volume_observer(_callback: Box<dyn Fn() + 'static>) {}

/// AROS exposes no sleep and wake events to a program: the callback is
/// dropped.
pub fn start_power_observer(_callback: Box<dyn Fn(PowerEvent) + 'static>) {}

/// No idle-sleep inhibition on AROS: `None`, which callers treat as
/// "could not inhibit".
pub fn prevent_idle_sleep(_reason: &str) -> Option<SleepBlocker> {
    None
}

// =============================================================
// Window
// =============================================================

/// The window handle is a macOS `NSView`; Intuition windows are driven by
/// the GPUI backend, and these docking primitives have no AROS form.
pub fn set_window_floating(_handle: *mut c_void, _floating: bool) {}

pub fn set_window_opacity(_handle: *mut c_void, _opacity: f32) {}

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
// Video overlay: no native player on AROS. Handle 0 = "no video".
// =============================================================

pub fn video_overlay_show(_path: &Path, _on_ended: Box<dyn Fn() + 'static>) -> u64 {
    0
}
pub fn video_overlay_copy_frame(_id: u64) -> Option<(u32, u32, Vec<u8>)> {
    None
}
pub fn video_overlay_remove(_id: u64) {}
pub fn video_overlay_set_paused(_id: u64, _paused: bool) {}
pub fn video_overlay_set_muted(_id: u64, _muted: bool) {}
pub fn video_overlay_restart(_id: u64) {}
pub fn video_overlay_time(_id: u64) -> (f64, f64) {
    (0.0, 0.0)
}
pub fn video_overlay_natural_size(_id: u64) -> (f64, f64) {
    (0.0, 0.0)
}
pub fn video_overlay_seek(_id: u64, _seconds: f64) {}
pub fn video_overlay_step(_id: u64, _frames: i64) {}

// =============================================================
// Privilege escalation and locked-file diagnostics: AROS has neither
// users with elevation nor a per-process open-file table.
// =============================================================

#[derive(Clone, Debug)]
pub struct LockingProcess {
    pub pid: u32,
    pub name: String,
}

pub fn elevation_available() -> bool {
    false
}

pub fn run_elevated_self(_args: &[String]) -> Result<i32, String> {
    Err("elevation is not available on AROS".into())
}

pub fn lock_diagnostics_available() -> bool {
    false
}

pub fn processes_using(_path: &Path) -> Vec<LockingProcess> {
    Vec::new()
}

#[derive(Clone, Debug, Default)]
pub struct LockScan {
    pub holders: Vec<LockingProcess>,
    pub scanned: usize,
    pub truncated: bool,
}

pub fn processes_using_tree(_roots: &[PathBuf], _max_files: usize) -> LockScan {
    LockScan::default()
}

pub fn force_close_processes(_pids: &[u32]) -> Result<(), String> {
    Err("force-closing processes is not available on AROS".into())
}

// =============================================================
// Path-backed platform roots: a Windows host concept (WSL).
// =============================================================

pub fn discover_path_backed_platform_roots(
    _cancel: &std::sync::atomic::AtomicBool,
) -> Vec<ferail_core::platform_locations::PathBackedPlatformRoot> {
    Vec::new()
}

pub fn activate_path_backed_platform_root(
    _id: &ferail_core::platform_locations::PlatformRootId,
    _cancel: &std::sync::atomic::AtomicBool,
) -> Result<PathBuf, ferail_core::platform_locations::PlatformRootErrorKind> {
    Err(ferail_core::platform_locations::PlatformRootErrorKind::Unavailable)
}

pub fn is_wsl_path(_path: &Path) -> bool {
    false
}

pub fn resolve_wsl_symlink_path(
    _path: &Path,
    _cancel: &std::sync::atomic::AtomicBool,
) -> Result<PathBuf, ferail_core::platform_locations::PlatformRootErrorKind> {
    Err(ferail_core::platform_locations::PlatformRootErrorKind::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dos_path_drops_the_slash_after_a_device_colon() {
        let c = dos_path(Path::new("SYS:/Prefs/Env-Archive")).unwrap();
        assert_eq!(c.to_bytes(), b"SYS:Prefs/Env-Archive");
        let c = dos_path(Path::new("RAM:T")).unwrap();
        assert_eq!(c.to_bytes(), b"RAM:T");
        let c = dos_path(Path::new("SYS:")).unwrap();
        assert_eq!(c.to_bytes(), b"SYS:");
    }

    #[test]
    fn compress_single_file_names_after_source_and_avoids_collision() {
        let dir = std::env::temp_dir().join(format!("ferail-shell-aros-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("data.bin");
        std::fs::write(&f, b"x").unwrap();
        let first = compress_paths(&[f.as_path()]).unwrap();
        assert_eq!(first.file_name().unwrap(), "data.bin.zip");
        let second = compress_paths(&[f.as_path()]).unwrap();
        assert_eq!(second.file_name().unwrap(), "data.bin (2).zip");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
