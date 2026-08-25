//! File-manager shell — main window content during Phases 4+.
//!
//! Phase 4.a: holds `current_dir`, renders a clickable breadcrumb at
//! the top of the main pane, sidebar entries are still placeholder.
//! Phase 4.b will wire the sidebar to real Locations/Volumes. Phase
//! 4.c brings the virtualized file list.

use crate::text::TextScale as _;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use ferail_core::favorites::FavoriteState;
use ferail_core::platform_namespace::{
    LocationTarget, PlatformBatchApply, PlatformCapabilities, PlatformItemId,
    PlatformListingRequest, PlatformLocation, PlatformLocationErrorKind, PlatformNamespaceProvider,
};
use ferail_core::{EntryKind, EnumerationError, FileEntry, NodeId};
use ferail_fs_native::{NativeFs, home_dir};
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, Root, Selectable, Sizable, TitleBar, WindowExt,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    sidebar::Sidebar,
    v_flex,
};

use crate::app_state;
use crate::file_list::FileListDelegate;
use crate::fs_watcher::{FsWatcher, POLL_INTERVAL, RELOAD_DEBOUNCE};
use crate::multi_table::{DataTable, TableEvent, TableState};
use crate::tasks::TaskKind;
use crate::tool_results::{ToolHostContext, ToolHostEvent};
use crate::tree::{ShellSidebarItem, TreeChild, TreeGuide, TreeRowIcon, TreeRowSpec, TreeSection};
use gpui::prelude::FluentBuilder as _;

mod actions;
mod checksum;
mod dock;
mod dupe_panel;
mod dupes;
mod file_ops;
pub use file_ops::ArchiveOpSettled;
pub(crate) use file_ops::pick_destination_folder;
pub(crate) use file_ops::{ArchiveSaveRequest, TransferMode};
mod loading;
mod lock_info;
mod path;
pub(crate) mod render;
mod search;
mod selection;
mod tab;

pub use actions::*;
pub use dock::{DockEdge, DockState, ScreenFrame};
use loading::{
    LoadBatch, LoadMsg, dir_has_subdir, error_copy, middle_truncate_path,
    run_directory_load_streaming, run_tree_children_load,
};
pub use path::{
    canonicalize_for_identity, parse_breadcrumb_path, parse_pasted_path, path_segments,
    resolve_go_to_target,
};
pub use tab::{ClosedTab, HistoryEntry, Tab, TabId, ToolResultSurface};

/// Callback the disk-usage view invokes to re-root the dock owner at a
/// new path. Boxed so the `Shell` can hand it out without naming itself.
type DiskUsageDockOwner = Rc<dyn Fn(PathBuf, Entity<crate::disk_usage::DiskUsageView>, &mut App)>;

/// Classification produced by `Shell::resolve_favorite_target` so
/// the toggle handler can show the appropriate toast for files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FavoriteResolved {
    Folder,
    NotAFolder,
}

#[derive(Clone, Debug)]
enum FileOpSuccessToast {
    None,
    /// Confirm only if the op ran long enough to surface a task row. Keeps
    /// trivial, obviously-completed actions (New Folder, Duplicate) from
    /// toasting every time.
    IfSurfaced(String),
    /// Always confirm, however fast it finished. For operations whose result
    /// is not visible where the user is looking — extraction writes its output
    /// into the destination folder, which may not be the folder on screen, so
    /// "did that work?" has no on-screen answer without this.
    Always(String),
}

#[derive(Clone, Debug)]
enum FileOpUndo {
    None,
    Rename { current: PathBuf, original: PathBuf },
    DeleteFolder(PathBuf),
    RemoveCreatedResult,
}

impl FileOpUndo {
    fn push(self, shell: &mut Shell, created: Vec<PathBuf>) {
        match self {
            FileOpUndo::None => {}
            FileOpUndo::Rename { current, original } => {
                shell.push_undo(UndoOp::Rename { current, original });
            }
            FileOpUndo::DeleteFolder(path) => {
                shell.push_undo(UndoOp::DeleteFolder(path));
            }
            FileOpUndo::RemoveCreatedResult => {
                if !created.is_empty() {
                    shell.push_undo(UndoOp::RemoveCreated(created));
                }
            }
        }
    }
}

/// Reversible operation pushed onto `Shell::undo_stack` after a
/// successful mutation. Filesystem variants apply synchronously via
/// [`UndoOp::apply_fs`]; favorites variants (`AddFavorite` /
/// `RemoveFavorite`) need Shell + cx and are handled inline by
/// `Shell::on_undo_last_action`.
#[derive(Clone, Debug)]
pub enum UndoOp {
    /// Rename `current` back to `original`.
    Rename { current: PathBuf, original: PathBuf },
    /// Delete the folder we just created.
    DeleteFolder(PathBuf),
    /// Undo an add: remove the favorite that was just created.
    AddFavorite(ferail_core::favorites::FavoriteId),
    /// Undo a remove: restore the captured favorite at its prior
    /// `sort_index`, with prior `display_name` / `custom_icon` /
    /// `date_added`. Identity (`FavoriteId`) is preserved so any
    /// toggle elsewhere stays consistent (§3.2).
    RemoveFavorite(ferail_core::favorites::Favorite),
    /// Undo a move: rename each `(from, to)` pair's `to` back to
    /// `from`. Only registered when every item took the same-volume
    /// rename path (docs/features/FILE_OPS.md); cross-volume moves
    /// register [`UndoOp::MoveBackCross`] instead.
    MoveBack(Vec<(PathBuf, PathBuf)>),
    /// Undo a cross-volume move: copy each `(original, moved)` pair's
    /// `moved` back to `original` (engine copy — same fidelity as the
    /// forward path), then delete `moved` only once its copy-back fully
    /// landed. Registered when the move replaced nothing (same
    /// eligibility spirit as `MoveBack`); a reoccupied `original`
    /// refuses per pair and keeps `moved` intact.
    MoveBackCross(Vec<(PathBuf, PathBuf)>),
    /// Undo a copy: delete the items it created. Only registered when
    /// the copy replaced nothing — undoing a replace would delete the
    /// sole remaining version.
    RemoveCreated(Vec<PathBuf>),
    /// Undo a move-to-trash: rename each `(original, trashed)` pair's
    /// trashed location back to its original. Pairs come from
    /// `trashItemAtURL`'s resulting URL [mac]; ops whose resulting
    /// URL wasn't reported (Windows Recycle Bin) don't register.
    TrashRestore(Vec<(PathBuf, PathBuf)>),
    /// Undo a bulk rename: rename each `(original, renamed)` pair's
    /// renamed location back to its original
    /// (docs/features/BULK_RENAME.md). Only pairs that actually renamed
    /// register — failed items never enter the record.
    RenameBatch(Vec<(PathBuf, PathBuf)>),
}

impl UndoOp {
    /// Apply filesystem-only variants. Favorites variants return
    /// `Err` here — the caller routes them through Shell + cx.
    fn apply_fs(&self) -> Result<(), String> {
        match self {
            UndoOp::Rename { current, original } => {
                std::fs::rename(current, original).map_err(|e| e.to_string())
            }
            UndoOp::DeleteFolder(p) => std::fs::remove_dir(p).map_err(|e| e.to_string()),
            UndoOp::MoveBack(pairs) => {
                for (from, to) in pairs {
                    // Same guard as TrashRestore: if something new
                    // appeared at the origin since the move, undoing
                    // must not clobber it.
                    if from.exists() {
                        return Err(tr!(
                            "{path} exists again; not overwriting it",
                            path = from.display()
                        )
                        .to_string());
                    }
                    std::fs::rename(to, from).map_err(|e| e.to_string())?;
                }
                Ok(())
            }
            UndoOp::MoveBackCross(pairs) => {
                // Per-pair copy-back; failures collect and the rest of
                // the batch continues (a reoccupied original must not
                // strand the other items' undo).
                let mut errors: Vec<String> = Vec::new();
                for (original, moved) in pairs {
                    if let Err(e) = copy_back_moved_item(original, moved) {
                        errors.push(e);
                    }
                }
                if errors.is_empty() {
                    Ok(())
                } else {
                    Err(errors.join("; "))
                }
            }
            UndoOp::RemoveCreated(paths) => {
                for p in paths {
                    let meta = std::fs::symlink_metadata(p).map_err(|e| e.to_string())?;
                    if meta.is_dir() && !meta.is_symlink() {
                        std::fs::remove_dir_all(p).map_err(|e| e.to_string())?;
                    } else {
                        std::fs::remove_file(p).map_err(|e| e.to_string())?;
                    }
                }
                Ok(())
            }
            UndoOp::TrashRestore(pairs) => {
                for (original, trashed) in pairs {
                    if original.exists() {
                        return Err(tr!(
                            "{path} exists again; not overwriting it",
                            path = original.display()
                        )
                        .to_string());
                    }
                    std::fs::rename(trashed, original).map_err(|e| e.to_string())?;
                }
                Ok(())
            }
            UndoOp::RenameBatch(pairs) => {
                // Reverse through the same chain/cycle-aware worker the
                // forward pass used — renumbering batches rename through
                // each other's names, so a naive in-order loop would trip
                // its own exists-guard. `rename_guarded` inside keeps
                // MoveBack's "don't clobber something new" contract per
                // item; failures collect instead of aborting the rest.
                let back: Vec<(PathBuf, PathBuf)> = pairs
                    .iter()
                    .map(|(original, renamed)| (renamed.clone(), original.clone()))
                    .collect();
                let (_, errors) = crate::bulk_rename::run_renames(back);
                if errors.is_empty() {
                    Ok(())
                } else {
                    Err(errors.join("; "))
                }
            }
            UndoOp::AddFavorite(_) | UndoOp::RemoveFavorite(_) => {
                Err("favorite undo handled by Shell".into())
            }
        }
    }

    fn label(&self) -> SharedString {
        match self {
            UndoOp::Rename { .. } => tr!("Undid rename"),
            UndoOp::DeleteFolder(_) => tr!("Removed new folder"),
            UndoOp::AddFavorite(_) => tr!("Removed Favorite"),
            UndoOp::RemoveFavorite(_) => tr!("Restored Favorite"),
            UndoOp::MoveBack(_) | UndoOp::MoveBackCross(_) => tr!("Moved items back"),
            UndoOp::RemoveCreated(_) => tr!("Removed pasted items"),
            UndoOp::TrashRestore(_) => tr!("Restored from Trash"),
            UndoOp::RenameBatch(_) => tr!("Undid bulk rename"),
        }
    }

    /// Parent directories whose listings change when this op applies —
    /// the reload set after a successful undo.
    fn affected_parents(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        let mut push = |p: Option<&std::path::Path>| {
            if let Some(p) = p.and_then(|p| p.parent()) {
                let p = p.to_path_buf();
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        };
        match self {
            UndoOp::Rename { current, original } => {
                push(Some(current));
                push(Some(original));
            }
            UndoOp::DeleteFolder(p) => push(Some(p)),
            UndoOp::MoveBack(pairs) | UndoOp::MoveBackCross(pairs) => {
                for (from, to) in pairs {
                    push(Some(from));
                    push(Some(to));
                }
            }
            UndoOp::RemoveCreated(paths) => {
                for p in paths {
                    push(Some(p));
                }
            }
            UndoOp::TrashRestore(pairs) => {
                for (original, trashed) in pairs {
                    push(Some(original));
                    push(Some(trashed));
                }
            }
            UndoOp::RenameBatch(pairs) => {
                for (original, renamed) in pairs {
                    push(Some(original));
                    push(Some(renamed));
                }
            }
            UndoOp::AddFavorite(_) | UndoOp::RemoveFavorite(_) => {}
        }
        out
    }
}

/// Undo one cross-volume moved item: copy `moved` back so it lands at
/// `original`, then — only once the copy-back fully succeeded — delete
/// `moved`. Synchronous filesystem work; runs inside `apply_fs`'s
/// background context, never the UI thread.
///
/// Never clobbers: a reoccupied `original` (or an occupied copy-back
/// name) refuses and leaves `moved` untouched. The copy rides the same
/// fs-native engine as the forward path (`plan_transfer` + `run_copy`),
/// so xattrs/sparseness/symlink stance survive the round trip.
fn copy_back_moved_item(original: &Path, moved: &Path) -> Result<(), String> {
    use ferail_fs_native::file_ops::{self as engine, CollisionPolicy, TransferProgress};

    // Same guard as MoveBack/TrashRestore: if something new appeared at
    // the origin since the move, undoing must not clobber it.
    if original.exists() {
        return Err(tr!(
            "{path} exists again; not overwriting it",
            path = original.display()
        )
        .to_string());
    }
    let Some(dest_dir) = original.parent() else {
        return Err(tr!(
            "{path}: no parent folder to restore into",
            path = original.display()
        )
        .to_string());
    };
    let prog = TransferProgress::new();
    let cancel = AtomicBool::new(false);
    let sources = [moved.to_path_buf()];
    let plan = engine::plan_transfer(&sources, dest_dir, &prog, &cancel)
        .map_err(|e| format!("{}: {e}", moved.display()))?;
    // Skip-on-collision: the engine must never replace anything during
    // an undo. `original` was just checked free; a Skip can still fire
    // when the forward move Keep-Both-renamed the item and something
    // unrelated now occupies the copy-back name.
    let out = engine::run_copy(&plan, &|_| CollisionPolicy::Skip, &prog, &cancel)
        .map_err(|e| format!("{}: {e}", moved.display()))?;
    if let Some(f) = out.failed.first() {
        return Err(f.to_string());
    }
    let Some((_, landed)) = out.created.first() else {
        // Skip policy fired — the copy-back destination is occupied.
        let occupied = dest_dir.join(moved.file_name().unwrap_or_default());
        return Err(tr!(
            "{path} exists again; not overwriting it",
            path = occupied.display()
        )
        .to_string());
    };
    if landed != original {
        // The forward move Keep-Both-renamed the item; restore the
        // original leaf name. On failure, drop the copy we just made
        // (it's a duplicate) and keep `moved` as the surviving version.
        if let Err(e) = std::fs::rename(landed, original) {
            let _ = remove_file_or_tree(landed);
            return Err(format!("{}: {e}", landed.display()));
        }
    }
    // Copy-back landed — delete the moved copy (symlink-safe, same
    // removal the forward cross-volume move uses).
    remove_file_or_tree(moved)
}

/// Symlink-safe removal of one path: a real directory removes
/// recursively, anything else (file or symlink — never followed)
/// removes as a file. Mirrors the forward move's source deletion.
fn remove_file_or_tree(path: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let removed = if meta.is_dir() && !meta.is_symlink() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    removed.map_err(|e| format!("{}: {e}", path.display()))
}

fn file_op_error_notification(
    operation: &str,
    err: &str,
) -> gpui_component::notification::Notification {
    let advice = crate::i18n::tr_static(classify_error_text(err).advice());
    error_notification(
        tr!(
            "{operation} failed: {detail}. {advice}",
            operation = operation,
            detail = err,
            advice = advice
        )
        .to_string(),
    )
}

/// Best-effort classification of a stringly-typed error message (the mutation
/// surfaces that still return `Result<_, String>` — duplicate, compress,
/// alias, rename) into a [`FileOpErrorKind`], so every surface shares the one
/// advice table the structured engine path uses.
fn classify_error_text(err: &str) -> ferail_fs_native::file_ops::FileOpErrorKind {
    use ferail_fs_native::file_ops::FileOpErrorKind as K;
    let lower = err.to_ascii_lowercase();
    if lower.contains("permission denied")
        || lower.contains("operation not permitted")
        || lower.contains("os error 1")
        || lower.contains("os error 13")
        || lower.contains("access is denied")
    {
        K::PermissionDenied
    } else if lower.contains("being used by another")
        || lower.contains("in use by")
        || lower.contains("sharing violation")
        || lower.contains("text file busy")
        || lower.contains("resource busy")
    {
        K::Locked
    } else if lower.contains("no such file or directory")
        || lower.contains("not found")
        || lower.contains("os error 2")
    {
        K::NotFound
    } else if lower.contains("file exists") || lower.contains("already exists") {
        K::AlreadyExists
    } else if lower.contains("no space left")
        || lower.contains("not enough space")
        || lower.contains("os error 28")
    {
        K::NoSpace
    } else if lower.contains("read-only file system") || lower.contains("read only") {
        K::ReadOnly
    } else if lower.contains("too long") || lower.contains("name too long") {
        K::NameTooLong
    } else {
        K::Other
    }
}

/// The most actionable failure kind in a batch — the one whose advice we show
/// and whose retry affordance (elevation / who's-locking) we offer. Permission
/// and lock failures win because the user can do something about them.
fn dominant_failure_kind(
    failed: &[ferail_fs_native::file_ops::FileOpError],
) -> ferail_fs_native::file_ops::FileOpErrorKind {
    use ferail_fs_native::file_ops::FileOpErrorKind as K;
    let priority = |k: K| match k {
        K::PermissionDenied => 0,
        K::Locked => 1,
        K::NoSpace => 2,
        K::ReadOnly => 3,
        K::NameTooLong => 4,
        K::AlreadyExists => 5,
        K::NotFound => 6,
        K::Other => 9,
    };
    failed
        .iter()
        .map(|e| e.kind)
        .min_by_key(|k| priority(*k))
        .unwrap_or(K::Other)
}

/// A transparent, per-item summary for a file op that partly failed:
/// "Move: 7 of 10 done · 3 failed", the first few items with their
/// plain-language reasons, then one piece of advice for the most actionable
/// failure. The raw OS detail rides along (it's what the Copy action carries).
pub(crate) fn file_op_outcome_summary(
    operation: &str,
    outcome: &ferail_fs_native::file_ops::OpOutcome,
) -> String {
    file_op_failure_report(
        operation,
        outcome.created.len(),
        outcome.skipped,
        &outcome.failed,
    )
}

/// The one structured "N of M · why" report every partly-failed mutation
/// surface shares (docs/features/FILE_OPS.md → "Resilient failures"):
/// the transfer path via [`file_op_outcome_summary`], and the trash /
/// Empty Trash / Clear Quarantine / tag-toggle workers directly, so they
/// all read identically and evolve in one place.
pub(crate) fn file_op_failure_report(
    operation: &str,
    done: usize,
    skipped: u64,
    failed: &[ferail_fs_native::file_ops::FileOpError],
) -> String {
    const SHOW: usize = 4;
    let total = done + failed.len() + skipped as usize;
    let mut msg = trn!(
        "{operation}: {done} of {total} done \u{00b7} {n} failed",
        "{operation}: {done} of {total} done \u{00b7} {n} failed",
        failed.len(),
        operation = operation,
        done = done,
        total = total
    )
    .to_string();
    for e in failed.iter().take(SHOW) {
        msg.push('\n');
        msg.push_str(&tr!(
            "\u{2022} {item} \u{2014} {reason}",
            item = e.item_label(),
            reason = crate::i18n::tr_static(e.kind.summary())
        ));
    }
    if failed.len() > SHOW {
        msg.push('\n');
        msg.push_str(&trn!(
            "\u{2022} \u{2026}and {n} more",
            "\u{2022} \u{2026}and {n} more",
            failed.len() - SHOW
        ));
    }
    msg.push('\n');
    msg.push_str(&crate::i18n::tr_static(
        dominant_failure_kind(failed).advice(),
    ));
    msg
}

/// An error notification that shows a one-line headline by default and can be
/// expanded to reveal the full message — so a long failure (a native error with
/// a path and a sentence of detail) is legible, not clipped to "it failed". A
/// **Copy** button always puts the *whole* message on the clipboard, handy for
/// pasting into a bug report. Both controls also keep the toast from
/// auto-hiding, so the user has time to read, expand, and copy.
pub(crate) fn error_notification(message: String) -> gpui_component::notification::Notification {
    use gpui_component::Sizable as _;
    use gpui_component::button::{Button, ButtonVariants as _};
    use gpui_component::notification::Notification;

    let (summary, has_more) = collapse_error_summary(&message);
    // Expand state shared between renders: the content builder runs on every
    // render and reads it; the toggle's click flips it and re-renders.
    let expanded = Rc::new(std::cell::Cell::new(false));

    Notification::error(summary)
        // No action button, so disable autohide explicitly (an action would
        // have done it implicitly).
        .autohide(false)
        .content(move |_note, _window, cx| {
            let is_expanded = expanded.get();
            let mut col = v_flex().gap_1().pt_1();

            // The full, untruncated message — only once the user expands it.
            if has_more && is_expanded {
                col = col.child(
                    div()
                        .id("error-detail")
                        .max_h(px(240.))
                        .overflow_y_scroll()
                        .text_scale_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(message.clone())),
                );
            }

            let mut row = h_flex().gap_2();
            if has_more {
                let toggle = expanded.clone();
                row = row.child(
                    Button::new("toggle-error-details")
                        .label(if is_expanded {
                            tr!("Hide details")
                        } else {
                            tr!("Details")
                        })
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |_note, _ev, _window, cx| {
                            toggle.set(!toggle.get());
                            cx.notify();
                        })),
                );
            }
            let for_copy = message.clone();
            row = row.child(
                Button::new("copy-error-message")
                    .label(tr!("Copy"))
                    .ghost()
                    .small()
                    .on_click(move |_, _, _| crate::platform_shell::copy_to_clipboard(&for_copy)),
            );

            col.child(row).into_any_element()
        })
}

/// Headline for an error toast: the first line, capped so a long path or
/// sentence doesn't blow the toast up before the user chooses to expand it.
/// Returns the summary and whether anything was hidden — i.e. whether to offer
/// a **Details** toggle at all (short, single-line errors need none).
fn collapse_error_summary(message: &str) -> (String, bool) {
    const MAX: usize = 140;
    let first_line = message.lines().next().unwrap_or("");
    let multiline = message.lines().nth(1).is_some();
    if !multiline && first_line.chars().count() <= MAX {
        (message.to_string(), false)
    } else {
        let mut summary: String = first_line.chars().take(MAX).collect();
        summary.push('\u{2026}');
        (summary, true)
    }
}

/// What a failure toast's Retry buttons need to re-run the failed items. The
/// notification's action/content builders run on every render, so this is
/// cloned per build (hence `Clone`).
#[derive(Clone)]
pub(crate) struct TransferRetry {
    pub shell: WeakEntity<Shell>,
    /// The failed top-level sources to re-attempt (not the ones that succeeded
    /// or were skipped).
    pub sources: Vec<PathBuf>,
    pub dest: PathBuf,
    /// The *resolved* mode (never `Auto`), so the retry behaves identically.
    pub mode: file_ops::TransferMode,
    /// At least one failure is a bare permission denial — elevation may help.
    pub elevation_recoverable: bool,
    /// The exact paths that failed as [`FileOpErrorKind::Locked`] (capped) —
    /// the ones "What's using it?" diagnoses via the platform lock lookup.
    pub locked: Vec<PathBuf>,
}

/// The transparent failure toast: the per-item "N of M · why" summary plus
/// actions to cope — **Copy** (raw detail → clipboard for a bug report),
/// in-process **Retry** of the failed items, and, when a permission failure
/// could be fixed by elevating and the platform supports it, **Retry as
/// administrator…**. Setting `.action` also keeps the toast from auto-hiding.
pub(crate) fn transfer_failure_notification(
    summary: String,
    retry: TransferRetry,
) -> gpui_component::notification::Notification {
    use gpui_component::Sizable as _;
    use gpui_component::button::{Button, ButtonVariants as _};
    use gpui_component::notification::Notification;

    let offer_admin = retry.elevation_recoverable && crate::platform_shell::elevation_available();
    let offer_lock =
        !retry.locked.is_empty() && crate::platform_shell::lock_diagnostics_available();

    // Primary action button (disables autohide): elevate when that can help,
    // otherwise a plain retry.
    let primary = retry.clone();
    let note = Notification::error(summary.clone()).action(move |_, _, cx| {
        let r = primary.clone();
        // Either button kicks off a fresh op that posts its own result toast,
        // so this failure toast dismisses itself once acted on rather than
        // lingering for the user to close.
        if offer_admin {
            Button::new("retry-elevated")
                .label(tr!("Retry as administrator\u{2026}"))
                .small()
                .on_click(cx.listener(move |note, _, window, cx| {
                    let _ = r.shell.update(cx, |shell, cx| {
                        shell.retry_transfer_elevated(
                            r.sources.clone(),
                            r.dest.clone(),
                            r.mode,
                            window,
                            cx,
                        );
                    });
                    note.dismiss(window, cx);
                }))
        } else {
            Button::new("retry-inproc")
                .label(tr!("Retry"))
                .small()
                .on_click(cx.listener(move |note, _, window, cx| {
                    let _ = r.shell.update(cx, |shell, cx| {
                        shell.spawn_transfer_op(
                            r.sources.clone(),
                            r.dest.clone(),
                            r.mode,
                            window,
                            cx,
                        );
                    });
                    note.dismiss(window, cx);
                }))
        }
    });

    // Secondary row: Copy always; a plain Retry too when the primary was the
    // elevated one (so the user can still try in-process without the prompt).
    let copy_msg = summary;
    let secondary = retry;
    note.content(move |_, _, cx| {
        let copy_msg = copy_msg.clone();
        let mut row = h_flex().gap_2().pt_1();
        row = row.child(
            Button::new("copy-failure")
                .label(tr!("Copy"))
                .ghost()
                .small()
                .on_click(move |_, _, _| crate::platform_shell::copy_to_clipboard(&copy_msg)),
        );
        if offer_admin {
            let r = secondary.clone();
            row = row.child(
                Button::new("retry-inproc-2")
                    .label(tr!("Retry"))
                    .ghost()
                    .small()
                    .on_click(cx.listener(move |note, _, window, cx| {
                        let _ = r.shell.update(cx, |shell, cx| {
                            shell.spawn_transfer_op(
                                r.sources.clone(),
                                r.dest.clone(),
                                r.mode,
                                window,
                                cx,
                            );
                        });
                        note.dismiss(window, cx);
                    })),
            );
        }
        // Locked items: name the holding processes (background lookup), then
        // offer close-and-retry from the follow-up toast.
        if offer_lock {
            let r = secondary.clone();
            row = row.child(
                Button::new("whats-using-it")
                    .label(tr!("What's using it?"))
                    .ghost()
                    .small()
                    .on_click(cx.listener(move |note, _, window, cx| {
                        let _ = r.shell.update(cx, |shell, cx| {
                            shell.inspect_locked_retry(r.clone(), window, cx);
                        });
                        note.dismiss(window, cx);
                    })),
            );
        }
        row.into_any_element()
    })
}

/// What a trash/delete failure toast's elevated retry needs. Like
/// [`TransferRetry`] but for the destructive ops, which have no destination.
/// Cloned per render (the action/content builders re-run).
#[derive(Clone)]
pub(crate) struct TrashRetry {
    pub shell: WeakEntity<Shell>,
    /// Just the *permission-denied* items — the only class elevation fixes.
    /// Empty when nothing is elevation-recoverable (then no admin button).
    pub sources: Vec<PathBuf>,
    /// `false` = move to the user's Trash; `true` = delete permanently.
    pub delete: bool,
}

/// A trash/delete failure toast. When at least one item failed on a bare
/// permission denial and the platform can elevate, it offers a primary
/// **"Move to Trash / Delete as administrator…"** button (re-runs just those
/// items as root) plus a **Copy** of the full detail. Otherwise it falls back
/// to the plain expandable/copyable [`error_notification`] — there's no
/// elevated recourse for a locked file or a vanished path.
pub(crate) fn trash_failure_notification(
    headline: String,
    copy_detail: String,
    retry: TrashRetry,
) -> gpui_component::notification::Notification {
    use gpui_component::Sizable as _;
    use gpui_component::button::{Button, ButtonVariants as _};
    use gpui_component::notification::Notification;

    let offer_admin = !retry.sources.is_empty() && crate::platform_shell::elevation_available();
    if !offer_admin {
        return error_notification(copy_detail);
    }

    let label = if retry.delete {
        tr!("Delete as administrator\u{2026}")
    } else {
        tr!("Move to Trash as administrator\u{2026}")
    };
    let primary = retry.clone();
    let note = Notification::error(headline).action(move |_, _, cx| {
        let r = primary.clone();
        Button::new("trash-elevated")
            .label(label.clone())
            .small()
            .on_click(cx.listener(move |note, _, window, cx| {
                let _ = r.shell.update(cx, |shell, cx| {
                    shell.retry_trash_elevated(r.sources.clone(), r.delete, window, cx);
                });
                // Retire this failure toast — the elevated retry posts its own
                // result toast (success / partial / cancelled), so leaving this
                // one up would just stack a stale dialog the user must close.
                note.dismiss(window, cx);
            }))
    });

    note.content(move |_, _, _| {
        let copy_detail = copy_detail.clone();
        h_flex()
            .gap_2()
            .pt_1()
            .child(
                Button::new("copy-trash-failure")
                    .label(tr!("Copy"))
                    .ghost()
                    .small()
                    .on_click(move |_, _, _| {
                        crate::platform_shell::copy_to_clipboard(&copy_detail)
                    }),
            )
            .into_any_element()
    })
}

fn enumeration_error_message(operation: &str, err: &EnumerationError) -> String {
    match err {
        EnumerationError::PermissionDenied => tr!(
            "{operation} failed: PermissionDenied. Grant Ferail access to this location or run with sufficient permissions, then try again.",
            operation = operation
        )
        .to_string(),
        EnumerationError::NotFound => tr!(
            "{operation} failed: NotFound. The folder may have moved, been deleted, or been unmounted. Refresh the parent location and try again.",
            operation = operation
        )
        .to_string(),
        EnumerationError::Other(raw) => tr!(
            "{operation} failed: {detail} (EnumerationError::Other). Refresh and try again; if it repeats, inspect the folder permissions or filesystem state.",
            operation = operation,
            detail = raw
        )
        .to_string(),
    }
}

/// Explicit WSL-link activation worker probe. This is called only from the
/// background executor after `readlink -f`; the annotation documents that the
/// blocking metadata syscall cannot migrate onto render/UI code.
#[allow(clippy::disallowed_methods)]
fn wsl_resolved_target_is_dir(path: &Path) -> bool {
    ferail_core::path_guard::assert_off_ui_thread("wsl resolved-target metadata");
    std::fs::metadata(path)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
}

const UNDO_STACK_CAP: usize = 20;

/// Key-context name for the Shell's outer container — same convention
/// gpui-component uses (e.g. `Root` / `Input`). The keymap module
/// drives every binding off `ferail_core::commands` as of Harvest
/// Stage 3; SHELL_CONTEXT gates them to the file-pane focus.
pub const SHELL_CONTEXT: &str = "Shell";

/// Phase 10: live System-Appearance follow. The macOS observer in
/// `crate::platform_shell::start_system_theme_observer` runs on the main
/// thread but has no `&mut App` — it can't call `Theme::change` itself.
/// Instead it pushes the latest dark-mode bool here; Shell::render
/// consumes the pending value (if any) and calls `Theme::change`
/// before painting. Single-digit-millisecond lag at worst.
static SYSTEM_THEME_PENDING: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(-1);

pub fn set_system_theme_pending(is_dark: bool) {
    SYSTEM_THEME_PENDING.store(
        if is_dark { 1 } else { 0 },
        std::sync::atomic::Ordering::Release,
    );
}

fn take_system_theme_pending() -> Option<bool> {
    let v = SYSTEM_THEME_PENDING.swap(-1, std::sync::atomic::Ordering::AcqRel);
    match v {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

pub fn init(cx: &mut App) {
    crate::multi_table::init(cx);
    crate::keymap::install(cx);
    crate::keymap::install_extras(cx);
    // Add highlight queries for grammars gpui-component ships without
    // one (C#, C, C++, Bash, Swift, CMake) so the preview pane colors
    // them. Process-global registry; runs once.
    crate::syntax_extra::register_extra_languages();
}

/// Per-segment breadcrumb child-folder cache: segment path → its child
/// folders (display name + path), or `None` while an enumeration is in
/// flight. See [`Shell::warm_breadcrumb_children`].
type BreadcrumbChildren = HashMap<PathBuf, Option<Rc<Vec<(SharedString, PathBuf)>>>>;

pub struct Shell {
    /// Process-scoped state shared with every other window of this
    /// process (today there is only one window, but the singleton
    /// is what the rest of the windows-instances-tabs spec is built
    /// on; see `crates/ferail-gpui/src/process_state.rs`).
    pub process: Rc<crate::process_state::ProcessState>,
    /// Open tabs in this window. Always non-empty; closing the last
    /// tab is rejected. Each tab owns its own `Entity<TableState>`,
    /// its own enumeration generation/cancel/task, and its own
    /// last-error / pending-select state — so tab-switching is
    /// instant and an inactive tab's enumeration keeps streaming.
    pub tabs: Vec<Tab>,
    /// Index of the active tab in `tabs`.
    pub active: usize,
    /// Focus handle for the Shell's key-context. Keybindings declared
    /// against `SHELL_CONTEXT` only fire when this handle (or one of
    /// its children) holds focus.
    pub focus_handle: FocusHandle,
    /// When true, dotfiles are shown in the list. Per-window today —
    /// future preference may make it per-tab.
    pub show_hidden: bool,
    /// Row index the user most recently right-clicked. Actions
    /// dispatched from the context menu read this; keyboard actions
    /// fall back to the active tab's `selected`. Cleared after each
    /// context-menu action handler runs.
    pub context_row: Option<usize>,
    /// Path target for the sidebar / breadcrumb / empty-space
    /// context menus (Phase 6). Set by the `.context_menu(...)`
    /// closure on right-click; consumed by `RevealContextPath` /
    /// `CopyContextPath` / `OpenContextInNewTab` /
    /// `NewFolderHere` handlers. Unlike `context_row` (which targets
    /// file-list rows by index), this carries the full path because
    /// sidebar items aren't part of the file list.
    pub context_target: Option<PathBuf>,
    /// Path target for Favorites mutations
    /// (docs/features/FAVORITES.md). Set by every "Add to Favorites" /
    /// "Remove from Favorites" context-menu closure and by row-row drag
    /// handlers; consumed (`take()`-style) by
    /// `on_toggle_favorite_for_target`. Fallback chain when unset:
    /// file-list selection → active tab `current_dir`.
    pub favorites_context_path: Option<PathBuf>,
    /// True while a breadcrumb crumb's right-click context menu is open,
    /// so the crumb's hover tooltip (the full path) is suppressed and
    /// doesn't collide with the menu. gpui-component's `context_menu`
    /// keeps its open/close state private with no callback
    /// (docs/GPUI-UPSTREAM.md), so we track it ourselves: set when the
    /// menu builder runs, cleared on the next left mouse-down at the
    /// shell root (which is also how the menu dismisses).
    pub breadcrumb_menu_open: bool,
    /// Off-thread-warmed child-folder lists per breadcrumb segment path,
    /// backing each segment's "Go to Subfolder" submenu. `None` == an
    /// enumeration is in flight (show "Loading…"); `Some(vec)` == done
    /// (possibly empty). Never `read_dir` on the menu/render path — the
    /// Prime Directive; the submenu reads this cache only.
    pub breadcrumb_children: BreadcrumbChildren,
    /// Spring-load dwell tracker: `(row_ix, first-hover time)` for a
    /// folder row a drag is currently hovering. After a short dwell the
    /// shell drills into that folder so the user can drop deeper without
    /// releasing (docs/features/FILE_OPS.md). Cleared on drill / drop.
    pub spring_load: Option<(usize, std::time::Instant)>,
    /// Spring-load dwell tracker for sidebar *tree* rows, keyed by path:
    /// hovering a collapsed folder with a drag expands it after a dwell
    /// so the user can drill the tree mid-drag.
    pub tree_spring: Option<(PathBuf, std::time::Instant)>,
    /// Whether the background-task panel popover is open. Toggled by
    /// clicking the task region in the status bar.
    pub task_panel_open: bool,
    /// CLI-injected status-bar progress override
    /// (`--simulate-progress`). `Some(_)` keeps the strip visible
    /// at that fraction (negative = indeterminate) regardless of
    /// `tasks` state — useful for screenshots.
    pub simulated_progress: Option<f32>,
    /// CLI-injected status-bar stats segment (`--simulate-stats`):
    /// renders a fixed "up · CPU · MEM · redraws/s" label so screenshots
    /// are deterministic (the real sampler never runs on the
    /// screenshot path).
    pub simulated_stats: bool,
    /// Cmd+L breadcrumb edit (Stage 9.b): when true the breadcrumb
    /// renders an Input field pre-filled with the active tab's
    /// current_dir instead of the clickable segments. Enter commits
    /// (canonicalise + navigate); Blur cancels.
    pub breadcrumb_editing: bool,
    /// `InputState` for the breadcrumb edit field. Constructed once
    /// at Shell creation; visible only while
    /// `breadcrumb_editing == true`.
    pub breadcrumb_input: Entity<InputState>,
    /// Stage 9.b: keyboard-shortcuts help overlay. `Some(filter)`
    /// while visible — the string is the live filter text shown in
    /// the modal's search input.
    pub shortcuts_help_filter: Option<String>,
    /// Input state for the shortcuts-help filter. Always allocated;
    /// only rendered when the overlay is visible.
    pub shortcuts_help_input: Entity<InputState>,
    /// Painted bounds of the toolbar's grid icon-size track, captured each
    /// render through a `canvas` so a cursor x maps back to a size. Zero
    /// until the track has painted once (it only exists in grid view, on a
    /// window wide enough to hold it).
    pub icon_size_track: Bounds<Pixels>,
    /// True while that track is being scrubbed. Held on the Shell rather
    /// than the track element so the drag keeps following the cursor after
    /// it leaves the 96-px bar — the same reason the viewer's adjustment
    /// sliders track their drag at panel level.
    pub icon_size_dragging: bool,
    /// Painted bounds for the two Similar Images criteria tracks. Like the
    /// icon-size track, these are captured during paint so click/scrub x
    /// positions map back to a value without a heavyweight widget.
    pub(crate) similar_structure_track: Bounds<Pixels>,
    pub(crate) similar_detail_track: Bounds<Pixels>,
    /// Active tab and criterion being scrubbed, if any. The tab id prevents
    /// a mid-gesture tab switch from changing the wrong result surface.
    pub(crate) similar_criteria_dragging: Option<(TabId, dupes::SimilarCriterion)>,
    /// Whether the right-side preview pane is visible. Cmd+P toggles
    /// it; Cmd+I focuses the preview's Get Info section (today it's
    /// the only thing in the pane).
    pub preview_visible: bool,
    /// Entry staged out of a docked archive for the preview pane: the scratch
    /// file to render and the entry's real name to label it with. The pane
    /// reads the *tab's* delegate, which is empty in archive mode (the
    /// workbench owns its own table), so without this it shows "No selection".
    /// Set by a docked archive workbench: what it wants previewed, in place of
    /// this tab's own selection (its rows live in the workbench's table, not
    /// the tab's). A workbench in its *own window* will host a panel directly
    /// instead, which is what the component split makes possible.
    pub preview_override: Option<crate::preview_panel::PreviewTarget>,
    /// The preview pane, as a component this Shell hosts. The archive
    /// workbench hosts its own, which is what lets a popped-out window preview
    /// its entries.
    pub preview_panel: Option<Entity<crate::preview_panel::PreviewPanel>>,
    /// Scroll position of the preview pane's body, so the content
    /// stays reachable (with a scrollbar) when the window is shorter
    /// than the metadata + actions stack. Persistent across renders;
    /// `Shell::preview` resets it on selection change so a new file
    /// starts at the top.
    pub preview_scroll: ScrollHandle,
    /// Scroll position of the inline text/code box nested inside the
    /// preview body. Tracked separately so its `offset()` / `max_offset()`
    /// are readable for wheel scroll-chaining: the box scrolls first, and
    /// only the residual past its top/bottom is forwarded to
    /// `preview_scroll` (see `Shell::preview`). Reset to top on selection
    /// change alongside `preview_scroll`.
    pub preview_text_scroll: ScrollHandle,
    /// Scroll position of the tab strip, so the row of tabs can scroll
    /// horizontally when there are more open tabs than fit the window.
    /// The chevron arrows in `Shell::tabstrip` page it; trackpad /
    /// shift-wheel scrolling rides `overflow_x_scroll` directly.
    /// Persistent across renders so the position — and the measured
    /// `max_offset` the arrows read — survive frame to frame.
    pub tab_scroll: ScrollHandle,
    /// Path the preview pane showed last frame — the selection-change
    /// edge detector for the scroll reset above.
    pub preview_scroll_path: Option<PathBuf>,
    /// UI zoom factor (Stage 9.b.5). 1.0 = default; bumped by Cmd+=
    /// and Cmd+-. Applied via `apply_ui_zoom`, which writes the
    /// gpui-component theme base font size (`theme.font_size`); `Root`
    /// pumps that into the window rem size every frame, and all text is
    /// rem-relative through the `crate::text` design tokens, so it
    /// scales as one. Icon/layout scaling is still TODO. Persisted in
    /// app_state.
    pub ui_scale: f32,
    /// Resizable-splitter state for the sidebar / center / preview
    /// columns. Persists across renders so the drag handles work as
    /// expected; sizes survive theme changes etc.
    pub splitter_state: Entity<crate::splitter::ResizableState>,
    /// Current sidebar width in DIPs (next-level Phase 5). Read from
    /// `app_state::sidebar_width` at construction (or the default
    /// 220), threaded into `resizable_panel().size(...)` on every
    /// render, and updated from the splitter's `on_resize` callback
    /// when the user drags the handle.
    pub sidebar_width: f32,
    /// Current preview pane width. Same lifecycle as `sidebar_width`.
    pub preview_width: f32,
    /// Height of the preview pane's thumbnail box in DIPs. Adjusted by
    /// dragging the resize grip under the image; persisted via
    /// `app_state::preview_thumb_height` on the same debounced save as
    /// the splitter widths.
    pub preview_thumb_h: f32,
    /// Live drag anchor for the thumbnail resize grip: the mouse y at
    /// drag start and `preview_thumb_h` at that moment. The grip's
    /// `on_drag_move` computes the new height from the absolute delta
    /// so the box tracks the cursor 1:1 regardless of pane scroll.
    /// Set by the grip's drag constructor; never cleared (a new drag
    /// overwrites it, and `on_drag_move` only fires mid-drag).
    pub preview_thumb_drag: Option<(Pixels, f32)>,
    /// True while a trailing splitter-width save is queued. The
    /// on_resize callback fires per drag tick; rather than write on each
    /// (and risk dropping the final value), the first tick arms a
    /// deferred write that reads the latest widths when it fires —
    /// guaranteeing the width at drag-end persists. See
    /// `schedule_splitter_save`.
    pub splitter_save_scheduled: bool,
    /// Sidebar collapsed to icons-only when true. Toggled by the
    /// SidebarToggleButton in the TitleBar; persisted via
    /// `app_state::sidebar_collapsed` so the choice survives
    /// restarts.
    pub sidebar_collapsed: bool,
    /// Favorites sidebar section collapsed (disclosure-triangle).
    /// Independent of the sidebar-wide icon-collapse; persisted via
    /// `MetadataDb::favorites_section_collapsed` so the choice
    /// survives restarts. Hydrated in `start_metadata_load`.
    pub favorites_section_collapsed: bool,
    /// Most-recently-focused favorite id. Set by clicks on a favorite
    /// row and by the arrow-key focus actions (§11.4); consumed by the
    /// keyboard-reorder (§4.4) and delete / activate actions so they
    /// operate on the row the user last touched. Drives the focus ring.
    /// `None` when no favorite has been focused this session.
    pub focused_favorite: Option<ferail_core::favorites::FavoriteId>,
    /// Focus handle for the Favorites section. When focused (a favorite
    /// row was clicked / arrowed into), the section's `FAVORITES_CONTEXT`
    /// key context routes Up/Down/Enter/Delete to the favorites focus
    /// actions instead of the file list (§11.4).
    pub favorites_focus: FocusHandle,
    /// Favorite ids that were *just added* (via the `Added` event) and
    /// should play the §2.2 fade-in on their next render. Populated by
    /// the favorites subscription, never by hydrate, so the list doesn't
    /// animate every row on launch. `with_animation` only plays once per
    /// element-id lifetime, so leaving an id here is harmless; it's
    /// pruned on remove.
    pub fav_appear: HashSet<ferail_core::favorites::FavoriteId>,
    /// Per-favorite dedup-pulse generation (§2.2). Bumped on every
    /// `DedupPulse` event; the render keys the pulse animation on the
    /// counter so a repeat dedup-add re-triggers the flash.
    pub fav_pulse: HashMap<ferail_core::favorites::FavoriteId, u32>,
    /// Favorites being removed with the §3.2 collapse animation. The row
    /// stays in the entity (rendering a fade+collapse) until its timer
    /// fires and drops it. Cleared on the actual `Removed` event.
    pub fav_removing: HashSet<ferail_core::favorites::FavoriteId>,
    /// Windows/Linux app menu bar (`gpui-component::AppMenuBar`).
    /// `Some(_)` only on non-macOS — those platforms have no global
    /// menu bar, so we render the menu strip in-window beneath the
    /// title bar. macOS uses `cx.set_menus()` for its NSApp menu and
    /// leaves this `None`. Reads from the same `cx.set_menus()`
    /// global state that the Mac path populates, so the menu spec
    /// has a single source of truth.
    pub menu_bar: Option<Entity<gpui_component::menu::AppMenuBar>>,
    /// Sidebar tree state (Stage 9.c): which directories are
    /// currently expanded. Updated on caret-click and by the
    /// `--expand <path>` CLI flag (which walks the path's ancestors).
    pub expanded: HashSet<PathBuf>,
    /// Cached direct-children of any path that's ever been expanded.
    /// Folders only (the tree shows hierarchy; files live in the
    /// main pane). Once cached, re-expand is instant; collapsing a
    /// folder doesn't evict its cache.
    pub tree_children: HashMap<PathBuf, Vec<TreeChild>>,
    /// Window-docking state (docs/features/DOCK.md). `Some` while the window
    /// is docked to a screen edge as an auto-hiding drawer; `None` when it's a
    /// normal window. Session-only — docking does not persist across launches
    /// (see the doc's deferral note). The geometry math lives in `dock.rs`.
    pub dock: Option<DockState>,
    /// The docked window's content `NSView`, captured (as a `usize`, since raw
    /// pointers aren't `Send`) when docking so the reveal poller — an async
    /// task with no `Window` handle — can move the window. `0` when undocked.
    pub dock_ns_view: usize,
    /// Generation guard for the reveal poll loop: docking bumps it and spawns a
    /// fresh loop; superseded loops see the mismatch and exit (same pattern as
    /// the viewer's auto-hide epoch).
    pub dock_poll_epoch: u64,
    /// Last OS-level window title pushed via `set_window_title`. The
    /// app's custom titlebar hides the native caption, but Windows
    /// Alt+Tab / the taskbar (and the macOS Window menu) still read it,
    /// so an empty caption shows a nameless entry. Synced from the
    /// active tab's folder at render; cached here so the platform call
    /// only fires when the text actually changed (one string compare
    /// per frame — see `sync_window_title`).
    last_window_title: String,
    /// Tracks the window's OS activation so the activation observer
    /// only force-refreshes folder sizes on a genuine background→
    /// foreground return (where a 3rd-party tool may have changed
    /// things the non-recursive watcher never saw), not on the initial
    /// launch activation or a same-state re-fire. Seeded `true`: the
    /// window starts active, so launch doesn't trigger a re-walk.
    was_window_active: bool,
    /// Type-to-select buffer for the file list / icon grid. Printable
    /// keystrokes typed with the list or grid focused accumulate here
    /// and jump the selection to the first entry whose display name
    /// starts with the buffer (Finder-style), scrolling it into view.
    /// The `Instant` is the last keystroke time; after
    /// `TYPEAHEAD_TIMEOUT` of idle the buffer resets so a fresh prefix
    /// starts clean. `None` until first use. See `on_typeahead_key`.
    typeahead: Option<(String, std::time::Instant)>,
    /// Live subscription handles (Input change, future watchers).
    /// Dropping them tears down the listeners — keep alongside the
    /// Shell so they outlive any frame.
    #[allow(dead_code)]
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    /// Immutable accessor for the active tab. Panics if tabs is
    /// empty — but the constructor + close_tab() invariant keep
    /// that from happening.
    #[inline]
    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// Mutable accessor for the active tab.
    #[inline]
    pub fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    /// Replace the active tab's filesystem surface with a provider-owned,
    /// pathless namespace surface. The provider itself (and therefore its
    /// identity arena) is owned by the tab session.
    pub fn open_platform_namespace(
        &mut self,
        provider: Arc<dyn PlatformNamespaceProvider>,
        initial: PlatformLocation,
        cx: &mut Context<Self>,
    ) -> Result<(), PlatformLocationErrorKind> {
        let mut session =
            crate::platform_namespace::PlatformNamespaceSession::new(provider, initial)?;
        let (request, cancel) = session.refresh();
        let tab_id = self.active_tab().id;
        let tab = self.active_tab_mut();
        if let Some(directory_cancel) = tab.load_cancel.take() {
            directory_cancel.store(true, Ordering::Relaxed);
        }
        tab.tool_result = None;
        tab.last_error = None;
        tab.platform_namespace = Some(session);
        self.start_platform_listing(tab_id, request, cancel, cx);
        cx.notify();
        Ok(())
    }

    fn click_platform_item(
        &mut self,
        tab_id: TabId,
        item_id: PlatformItemId,
        toggle: bool,
        activate: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        let target = {
            let Some(session) = self.tabs[index].platform_namespace.as_mut() else {
                return;
            };
            if toggle {
                session.selection_mut().toggle(item_id);
            } else {
                session.selection_mut().select_only(item_id);
            }
            activate.then(|| {
                session
                    .store()
                    .items()
                    .iter()
                    .find(|item| item.id == item_id)
                    .filter(|item| item.capabilities.contains(PlatformCapabilities::OPEN))
                    .map(|item| item.target.clone())
            })
        }
        .flatten();

        match target {
            Some(LocationTarget::FileSystem(path)) => self.load_path_for_tab(tab_id, path, cx),
            Some(LocationTarget::Platform(location)) => {
                let request = self.tabs[index]
                    .platform_namespace
                    .as_mut()
                    .and_then(|session| session.navigate_to(location).ok());
                if let Some((request, cancel)) = request {
                    self.start_platform_listing(tab_id, request, cancel, cx);
                }
            }
            None => cx.notify(),
        }
    }

    fn navigate_platform_location(
        &mut self,
        tab_id: TabId,
        location: PlatformLocation,
        cx: &mut Context<Self>,
    ) {
        let request = self
            .tabs
            .iter_mut()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| tab.platform_namespace.as_mut())
            .and_then(|session| session.navigate_to(location).ok());
        if let Some((request, cancel)) = request {
            self.start_platform_listing(tab_id, request, cancel, cx);
        }
    }

    fn start_platform_listing(
        &mut self,
        tab_id: TabId,
        request: PlatformListingRequest,
        cancel: Arc<AtomicBool>,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| tab.platform_namespace.as_ref())
            .map(|session| session.provider())
        else {
            return;
        };
        let (sender, receiver) =
            async_channel::bounded(crate::platform_namespace::PLATFORM_PENDING_BATCHES_MAX);
        cx.background_executor()
            .spawn(async move {
                crate::platform_namespace::run_provider_stream(provider, request, cancel, sender);
            })
            .detach();

        cx.spawn(async move |this, cx| {
            while let Ok(event) = receiver.recv().await {
                let terminal = matches!(
                    &event,
                    crate::platform_namespace::PlatformListingEvent::Finished { .. }
                );
                let stale = this
                    .update(cx, |this, cx| {
                        let Some(index) = this.tabs.iter().position(|tab| tab.id == tab_id) else {
                            return true;
                        };
                        let Some(session) = this.tabs[index].platform_namespace.as_mut() else {
                            return true;
                        };
                        let stale = match event {
                            crate::platform_namespace::PlatformListingEvent::Batch(batch) => {
                                session.apply_batch(batch) != PlatformBatchApply::Applied
                            }
                            crate::platform_namespace::PlatformListingEvent::Finished {
                                request,
                                result,
                            } => {
                                session.finish_provider(&request, result);
                                false
                            }
                        };
                        // A background tab stores its bounded result but does
                        // not redraw the active surface for invisible batches.
                        if index == this.active {
                            cx.notify();
                        }
                        stale
                    })
                    .unwrap_or(true);
                if stale || terminal {
                    break;
                }
            }
        })
        .detach();
    }

    #[inline]
    pub fn current_node(&self) -> NodeId {
        self.active_tab().nav.current()
    }

    /// Keep the OS-level window caption in step with the active tab's
    /// folder. The custom titlebar hides the native caption, but the
    /// Windows taskbar / Alt+Tab switcher (and the macOS Window menu)
    /// still read it — without this the window shows up nameless when
    /// switching tasks. Called from render; the cached `last_window_title`
    /// makes the per-frame cost a single string compare and the platform
    /// `SetWindowTextW`/`setTitle:` call only fires on change.
    ///
    /// Reads only in-memory state (the cached `current_dir` PathBuf), so
    /// it honours the no-I/O-on-the-UI-thread Prime Directive.
    fn sync_window_title(&mut self, window: &mut Window) {
        let title = window_title_for(&self.active_tab().current_dir);
        if title != self.last_window_title {
            window.set_window_title(&title);
            self.last_window_title = title;
        }
    }
}

/// The OS window caption for a folder: its basename plus the app name,
/// e.g. `Documents — Ferail`. Roots like `C:\` or `/` have an empty
/// `file_name()`, so fall back to the full path; a fully empty path
/// yields the bare app name. The app name is always present so the
/// Windows Alt+Tab / taskbar entry is never blank.
fn window_title_for(dir: &Path) -> String {
    // The OS title bar is drawn by the host's native title font. On AROS
    // that is the Intuition topaz bitmap font (ASCII only), which renders the
    // em-dash as garbage — use a plain hyphen there.
    #[cfg(target_os = "aros")]
    const SEP: &str = " - ";
    #[cfg(not(target_os = "aros"))]
    const SEP: &str = " \u{2014} ";
    match dir.file_name() {
        Some(name) if !name.is_empty() => {
            // Finder-parity leaf: a folder stored with `:` titles as `/` on macOS.
            let shown =
                ferail_fs_native::paths::display_leaf(name.to_string_lossy().as_ref()).into_owned();
            format!("{shown}{SEP}Ferail")
        }
        _ => {
            // A root with no leaf (e.g. `C:\`): show the clean display form,
            // never the canonicalized `\\?\C:\`.
            let path = ferail_fs_native::paths::display_path(dir);
            if path.is_empty() {
                "Ferail".to_string()
            } else {
                format!("{path}{SEP}Ferail")
            }
        }
    }
}

/// When Back lands on an ancestor of the folder we just left, select the
/// immediate child folder under that destination. Examples:
/// `/A/B` -> `/A` selects `/A/B`; `/A/B/C` -> `/A` selects `/A/B`.
fn history_child_to_select(from: &Path, destination: &Path) -> Option<PathBuf> {
    if from == destination {
        return None;
    }
    let relative = from.strip_prefix(destination).ok()?;
    relative.components().find_map(|component| {
        if let std::path::Component::Normal(name) = component {
            Some(destination.join(name))
        } else {
            None
        }
    })
}

#[derive(Copy, Clone)]
enum SelectionDelta {
    Up,
    Down,
    PageUp,
    PageDown,
    First,
    Last,
}

fn now_unix_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Pull existing Ant Trail visit counts out of the metadata DB so
/// heat is reflected on the very first render. Returns
/// `(empty_map, 0)` when the DB is absent or the read fails — heat
/// tint just won't show until the user's done some navigating.
fn hydrate_ant_trail(
    db: Option<&Arc<Mutex<ferail_meta::MetadataDb>>>,
) -> (HashMap<PathBuf, u32>, u32, Vec<PathBuf>) {
    let Some(db) = db else {
        return (HashMap::new(), 0, Vec::new());
    };
    let Ok(guard) = db.lock() else {
        return (HashMap::new(), 0, Vec::new());
    };
    let Ok(entries) = guard.load_ant_trail() else {
        return (HashMap::new(), 0, Vec::new());
    };
    // Recents = the same visit log ordered by last access (the heat
    // map ignores time, so derive it separately from the same rows).
    // `last_access_unix == 0` is the cleared sentinel — those rows may
    // still carry heat (`hits`) but are no longer recent, so skip them.
    let mut by_recency: Vec<(PathBuf, i64)> = entries
        .iter()
        .filter(|e| e.last_access_unix > 0)
        .map(|e| (PathBuf::from(&e.folder_path), e.last_access_unix))
        .collect();
    by_recency.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
    let recents: Vec<PathBuf> = by_recency
        .into_iter()
        .take(crate::process_state::RECENTS_CAP)
        .map(|(p, _)| p)
        .collect();
    let mut max: u32 = 0;
    let mut map: HashMap<PathBuf, u32> = HashMap::with_capacity(entries.len());
    for e in entries {
        if e.hits > max {
            max = e.hits;
        }
        map.insert(PathBuf::from(e.folder_path), e.hits);
    }
    (map, max, recents)
}

/// Open the persistent metadata DB at the platform default location
/// (`ferail_meta::default_db_path`: Application Support on macOS,
/// %APPDATA% on Windows, XDG data dir elsewhere). Returns `None` and
/// logs a warning when the base env var is unset, mkdir fails, or
/// open fails — in-memory state still works in those cases, just
/// without persistence. Reuses the path resolution + parent-dir
/// helpers from `ferail_meta`.
fn open_metadata_db() -> Option<Arc<Mutex<ferail_meta::MetadataDb>>> {
    let Some(path) = ferail_meta::default_db_path() else {
        crate::log_warn!(
            90,
            "metadata: no base dir (HOME/APPDATA/XDG unset); persistence disabled"
        );
        return None;
    };
    if let Err(e) = ferail_meta::ensure_parent_dir(&path) {
        crate::log_warn!(90, "metadata: mkdir failed for {}: {e}", path.display());
        return None;
    }
    match ferail_meta::MetadataDb::open(&path) {
        Ok(db) => {
            crate::log_info!(90, "metadata: opened {}", path.display());
            Some(Arc::new(Mutex::new(db)))
        }
        Err(e) => {
            crate::log_warn!(90, "metadata: open failed for {}: {e}", path.display());
            None
        }
    }
}

/// Open a fresh Shell window already navigated to `path`. Backs the
/// Cmd+Option-click "open favorite in a new window" gesture (§11.3) —
/// reuses the singleton [`crate::process_state::ProcessState`] so the
/// new window shares favorites, caches, and the watcher with every
/// other window. The window-options shape mirrors `main.rs`'s
/// `open_shell_window_sized`.
pub fn open_window_at(cx: &mut App, path: PathBuf) {
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1180.0), px(760.0)), cx)),
        ..crate::shell_window_options()
    };
    let _ = cx.open_window(opts, |window, cx| {
        crate::boot::install_dev_window_callback_cleanup(window, cx);
        let process = crate::process_state::process_state(cx);
        let view = cx.new(|cx| Shell::new(process, window, cx));
        view.update(cx, |shell, cx| {
            shell.load_path(path.clone(), cx);
        });
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    });
}

/// Reveal `path` in a Ferail file window from a non-Shell context — the
/// Settings window's Diagnostics "Reveal" buttons. Finder-style reveal:
/// navigate to the parent folder with the entry queued for selection
/// (scrolled into view once rows land). The first live Shell window gets
/// a new tab and is raised; with none open, a fresh window opens at the
/// parent. A target that doesn't exist yet ("not created yet" rows)
/// still opens the parent — the unresolved name is dropped when the
/// load completes.
pub fn reveal_path_in_app(cx: &mut App, path: PathBuf) {
    let (dir, names) = match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => (
            parent.to_path_buf(),
            vec![name.to_string_lossy().into_owned()],
        ),
        // A filesystem root (or a bare relative leaf): open it directly.
        _ => (path.clone(), Vec::new()),
    };
    open_in_app(cx, dir, names);
}

/// Open the folder `dir` itself in a Ferail tab — the Settings window's
/// Bug-reports folder buttons. Same window policy as
/// [`reveal_path_in_app`] (new tab in a live Shell window, else a fresh
/// window), but lands *inside* the folder instead of selecting it in
/// its parent.
pub fn open_dir_in_app(cx: &mut App, dir: PathBuf) {
    open_in_app(cx, dir, Vec::new());
}

fn open_in_app(cx: &mut App, dir: PathBuf, names: Vec<String>) {
    // Prefer an existing Shell window: new tab there, then raise it. The
    // windows list also holds Settings/viewer Roots — the downcast skips
    // them.
    for handle in cx.windows() {
        let Some(root) = handle.downcast::<gpui_component::Root>() else {
            continue;
        };
        let revealed = root
            .update(cx, |root, window, cx| {
                let Ok(shell) = root.view().clone().downcast::<Shell>() else {
                    return false;
                };
                shell.update(cx, |s, cx| {
                    s.reveal_in_new_tab(dir.clone(), names.clone(), window, cx);
                });
                window.activate_window();
                true
            })
            .unwrap_or(false);
        if revealed {
            return;
        }
    }
    // No Shell window open (Settings can outlive the last one): open a
    // fresh window at the parent with the selection queued.
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1180.0), px(760.0)), cx)),
        ..crate::shell_window_options()
    };
    let _ = cx.open_window(opts, |window, cx| {
        crate::boot::install_dev_window_callback_cleanup(window, cx);
        let process = crate::process_state::process_state(cx);
        let view = cx.new(|cx| Shell::new(process, window, cx));
        view.update(cx, |shell, cx| {
            shell.load_path(dir.clone(), cx);
            if !names.is_empty() {
                shell.active_tab_mut().pending_select_names = names.clone();
            }
        });
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    });
}

/// One of the macOS-standard sidebar destinations shown in the
const ICON_WARM_CHUNK: usize = 16;
const ICON_WARM_INTERVAL: Duration = Duration::from_millis(16);

/// How often the splitter's drag callback is allowed to write the
/// app_state config file. 500 ms means a continuous drag samples ~2
/// times per second to disk; the final width at drag-end persists
/// because the next render re-checks the interval and flushes.
const SPLITTER_PERSIST_INTERVAL: Duration = Duration::from_millis(500);

/// How long a fresh navigation may go without its first enumeration
/// batch before the pane flips into the skeleton loading view
/// (`FileListDelegate::slow_load`). Local disks answer in a few
/// milliseconds and never show it; a spun-down external drive or cold
/// network mount takes seconds and gets in-pane feedback instead of
/// the previous directory's stale rows. Showing it with no delay
/// would flash a skeleton on every ordinary navigation.
const SLOW_LOAD_INDICATOR_DELAY: Duration = Duration::from_millis(300);

const SIDEBAR_COLLAPSED_WIDTH: f32 = 48.0;
const SIDEBAR_MIN_WIDTH: f32 = 160.0;
const SIDEBAR_MAX_WIDTH: f32 = 400.0;
const FILE_PANE_MIN_WIDTH: f32 = 360.0;
const PREVIEW_MIN_WIDTH: f32 = 260.0;
const PREVIEW_MAX_WIDTH: f32 = 640.0;
/// Height range for the preview pane's thumbnail box, adjusted by the
/// drag grip under the image (`Shell::preview`). Matches the clamp
/// `app_state` applies at load so a stale persisted value can't wedge
/// the pane.
pub(crate) const PREVIEW_THUMB_MIN_H: f32 = 120.0;
pub(crate) const PREVIEW_THUMB_MAX_H: f32 = 600.0;

impl Shell {
    /// Construct the singleton `ProcessState` for this process.
    /// Called exactly once from `main.rs` (or the screenshot harness)
    /// before any window opens; the resulting `Rc` is stashed as a
    /// GPUI `Global` and read back by every `Shell::new`.
    pub fn build_process_state(cx: &mut App) -> Rc<crate::process_state::ProcessState> {
        let fs = Arc::new(NativeFs::new());
        // Spin up the platform file-system watcher. Errors
        // (sandboxed CI without FSEvents) are non-fatal — the app
        // still runs, just without live external updates. Safe mode
        // skips it entirely (no notify backend, no fs-watcher thread);
        // every watcher consumer already tolerates `None`.
        let watcher_rc: Rc<RefCell<Option<FsWatcher>>> = if crate::safe_mode::enabled() {
            Rc::new(RefCell::new(None))
        } else {
            match FsWatcher::new() {
                Ok(w) => Rc::new(RefCell::new(Some(w))),
                Err(_) => Rc::new(RefCell::new(None)),
            }
        };
        // Favorites is process-scoped: one Entity shared across every
        // window. DB handle attached later by `start_metadata_load`.
        let favorites = cx.new(|_| crate::favorites::Favorites::new(None));
        crate::process_state::ProcessState::new(fs, watcher_rc, favorites)
    }

    pub fn new(
        process: Rc<crate::process_state::ProcessState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let persisted = app_state::load();
        // Recents disclosure state is process-wide; seed it from the
        // persisted flag once (the first window to construct wins; a
        // later toggle re-persists).
        process
            .recents_section_collapsed
            .set(persisted.recents_collapsed.unwrap_or(false));
        // Persisted state is an external boundary for the path-identity
        // contract: the raw stored path must be validated and
        // re-canonicalized before it is trusted (a symlinked spelling
        // saved last session must not mint a second NodeId; a vanished
        // directory falls back to home). Both of those are disk I/O —
        // `is_dir` + `canonicalize` block for seconds on a spun-down
        // external drive or a network mount — and this runs on the UI
        // thread at *every* window open (Cmd+N used to freeze for the
        // duration). Prime Directive: the window boots on the raw
        // spelling immediately and `resolve_start_path_then_load` does
        // the I/O on the background executor, then loads the canonical
        // path (or home). No persisted last-dir → home, which needs no
        // I/O to trust, so that case loads straight away.
        let (start, start_needs_resolution) = match persisted.last_dir.clone() {
            Some(p) => (p, true),
            None => (home_dir(), false),
        };
        let start_id = process.fs.id_for_path(&start);
        // Seed the NodeStore with the start path so the very first
        // navigate doesn't re-mint a different NodeId. Idempotent —
        // the second window seeing the same path is a no-op.
        process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(start.clone(), start_id);
        // Add this window's start path to the shared watcher set.
        // Each visible tab registers its directory; watcher events are
        // fanned out to every matching tab in every live window.
        if let Some(w) = process.watcher.borrow_mut().as_mut() {
            w.watch(&start);
        }
        let show_hidden = persisted.show_hidden.unwrap_or(false);
        // Safe mode forces the three background-scan switches off for
        // this session, whatever was persisted — freeze bisection needs
        // a launch with no ambient disk work. The persisted values are
        // untouched; a normal relaunch restores them.
        let safe_mode = crate::safe_mode::enabled();
        // Seed the live thumbnail toggle from persisted settings so the
        // file list and Settings window agree from the first frame.
        cx.set_global(crate::thumbnails::ShowThumbnails(
            persisted.show_thumbnails.unwrap_or(true) && !safe_mode,
        ));
        // Seed the live grid icon size from persisted settings.
        cx.set_global(crate::grid::IconSize(crate::grid::clamp_icon_size(
            persisted
                .icon_size
                .unwrap_or(crate::grid::DEFAULT_ICON_SIZE),
        )));
        // Seed the live grid selection-gutter size from persisted settings.
        cx.set_global(crate::grid::CellGap(crate::grid::clamp_cell_gap(
            persisted.cell_gap.unwrap_or(crate::grid::DEFAULT_CELL_GAP),
        )));
        // Seed the live grid thumbnail fit mode from persisted settings.
        cx.set_global(crate::grid::ThumbFitMode(crate::grid::ThumbFit::from_str(
            persisted.thumb_fit.as_deref().unwrap_or("best"),
        )));
        // Seed the folder-size walker master switch. Default true (on).
        cx.set_global(crate::folder_sizes::FolderSizingEnabled(
            persisted.folder_sizing.unwrap_or(true) && !safe_mode,
        ));
        // Seed the per-row file-detail scan switch (magic sniff + Finder
        // tags). Default true (on).
        cx.set_global(crate::prefetch::FileDetailScan(
            persisted.file_detail_scan.unwrap_or(true) && !safe_mode,
        ));
        // Seed the live selection accent (file list + grid share it).
        // `None` ⇒ the helpers fall back to the theme's blue.
        cx.set_global(crate::selection_colors::SelectionAccent(
            persisted
                .selection_color
                .as_deref()
                .and_then(crate::selection_colors::parse_hex),
        ));
        // Seed the Ant Trail master switch. Default true (tint on).
        cx.set_global(crate::ant_trail::AntTrailEnabled(
            persisted.ant_trail_enabled.unwrap_or(true),
        ));
        // Seed the live Ant Trail tint (list + grid share it). `None` ⇒
        // the original warm orange via `ant_trail::default_base`.
        cx.set_global(crate::ant_trail::AntTrailColor(
            persisted
                .ant_trail_color
                .as_deref()
                .and_then(crate::selection_colors::parse_hex),
        ));
        // Seed the favorites-tracking policy. Default true (exclude).
        cx.set_global(crate::ant_trail::ExcludeFavoritesFromTracking(
            persisted.exclude_favorites_from_tracking.unwrap_or(true),
        ));
        // Seed the Recents master switch. Default true (on).
        cx.set_global(crate::recents_section::RecentsEnabled(
            persisted.recents_enabled.unwrap_or(true),
        ));
        // FERAIL_UI_SCALE env var (regression tool / screenshots)
        // wins over the persisted value when set. Both are clamped.
        let ui_scale = std::env::var("FERAIL_UI_SCALE")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .or(persisted.ui_scale)
            .map(|n| n.clamp(0.6, 2.0))
            .unwrap_or(1.0);
        let focus_handle = cx.focus_handle();
        // Grab focus on first paint so the Backspace keybind works
        // immediately without the user having to click into the
        // shell.
        focus_handle.focus(window, cx);

        // Stage 9.b: shortcuts-help filter Input. Subscribed for
        // Change so typing updates `shortcuts_help_filter` live.
        let shortcuts_help_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("Search\u{2026}")));
        // The callback reads the input through the `state` parameter the
        // subscription hands back — capturing a strong Entity clone here
        // instead is a self-cycle (the App holds the listener for as long as
        // the entity lives, and the listener would hold the entity), which
        // surfaced as GPUI's leaked-handle assertion on Windows quit.
        let shortcuts_help_subscription = cx.subscribe_in(
            &shortcuts_help_input,
            window,
            move |this, state, ev: &InputEvent, window, cx| {
                let Some(filter) = this.shortcuts_help_filter.clone() else {
                    return;
                };
                match ev {
                    InputEvent::Change => {
                        let v = state.read(cx).value().to_string();
                        this.shortcuts_help_filter = Some(v);
                        cx.notify();
                    }
                    // Enter runs the highlighted top match — turns the
                    // shortcuts overlay into a keyboard-driven command
                    // palette (filter, Enter). Arrow-key selection
                    // between matches is a follow-up.
                    InputEvent::PressEnter { .. } => {
                        if let Some(action) = crate::keyboard_help::palette_top_action(&filter) {
                            this.close_shortcuts_help(window, cx);
                            window.dispatch_action(action, cx);
                        }
                    }
                    _ => {}
                }
            },
        );

        // Stage 9.b: breadcrumb-edit Input. Subscribed for
        // PressEnter (commit) and Blur (cancel). The completion
        // provider gives Cmd+L folder autocomplete: matching child
        // folders pop up as you type (background-enumerated; see
        // `crate::path_complete`).
        let breadcrumb_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder(tr!("/path/to/folder"));
            state.lsp.completion_provider =
                Some(Rc::new(crate::path_complete::PathCompletionProvider));
            state
        });
        // Same as above: read through the `state` parameter, never a
        // captured strong clone of the subscribed entity.
        let breadcrumb_subscription = cx.subscribe_in(
            &breadcrumb_input,
            window,
            move |this, state, ev: &InputEvent, _window, cx| match ev {
                InputEvent::PressEnter { .. } => {
                    let raw = state.read(cx).value().to_string();
                    crate::log_info!(90, "breadcrumb: commit {raw:?}");
                    let path = parse_breadcrumb_path(&raw);
                    this.breadcrumb_editing = false;
                    // External boundary: typed input canonicalizes on
                    // a worker before navigation registers identity.
                    this.navigate_external(path, cx);
                }
                InputEvent::Blur if this.breadcrumb_editing => {
                    this.breadcrumb_editing = false;
                    cx.notify();
                }
                _ => {}
            },
        );

        // Foreground-executor polling task. Wakes every POLL_INTERVAL,
        // drains the channel, asks the Shell to reload if anything
        // changed. Stops when this.update returns Err — that means
        // the Shell entity has been dropped.
        let poll_watcher = process.watcher.clone();
        let poll_process = process.clone();
        cx.spawn(async move |this, cx| {
            // Per-directory reload throttle (leading edge). Pending dirty
            // paths carry across polls; a directory reloads at once on its
            // first event, then at most once per RELOAD_DEBOUNCE while events
            // keep arriving — coalescing FSEvents bursts without ever dropping
            // a change (see `RELOAD_DEBOUNCE`).
            let mut last_reload: std::collections::HashMap<PathBuf, std::time::Instant> =
                std::collections::HashMap::new();
            let mut pending: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if let Some(w) = poll_watcher.borrow().as_ref() {
                    pending.extend(w.drain_reload_relevant_paths());
                }
                if pending.is_empty() {
                    continue;
                }
                let now = std::time::Instant::now();
                // Surface only the paths whose throttle window has elapsed;
                // the rest stay pending for a later poll (no change is lost).
                let mut due: Vec<PathBuf> = Vec::new();
                pending.retain(|p| {
                    let ready = last_reload
                        .get(p)
                        .is_none_or(|t| now.duration_since(*t) >= RELOAD_DEBOUNCE);
                    if ready {
                        due.push(p.clone());
                        false
                    } else {
                        true
                    }
                });
                if due.is_empty() {
                    continue;
                }
                for p in &due {
                    last_reload.insert(p.clone(), now);
                }
                // Drop throttle entries for directories quiet for a while so
                // the map can't grow unbounded over a long session.
                last_reload.retain(|_, t| now.duration_since(*t) < RELOAD_DEBOUNCE * 8);
                if this.update(cx, |_, _| {}).is_err() {
                    break;
                }
                Shell::broadcast_reload_for_process(&poll_process, due, cx);
            }
        })
        .detach();

        // Relative-time tick. The Modified column renders "4 seconds ago"
        // style labels recomputed from `mtime_unix` each paint, so the
        // column only counts forward if something repaints. This loop
        // requests a repaint on an *adaptive* cadence driven by the
        // freshest row in the active tab:
        //   - newest file < 1 h old  → 1 s   (a seconds component is shown)
        //   - otherwise              → 60 s  (minute/hour/day granularity)
        // and it skips the repaint entirely once every visible row is a
        // week or more old, where the label is a static date — so browsing
        // an old folder costs nothing. A new file arriving re-enumerates
        // and repaints through the watcher path, which re-arms fine ticking.
        cx.spawn(async move |this, cx| {
            let mut delay = std::time::Duration::from_secs(1);
            loop {
                cx.background_executor().timer(delay).await;
                let next = this.update(cx, |this, cx| {
                    let now = ferail_core::now_unix();
                    let freshest_age = this
                        .active_tab()
                        .table
                        .read(cx)
                        .delegate()
                        .entries
                        .iter()
                        .map(|e| e.mtime_unix)
                        .max()
                        .map(|newest| now - newest)
                        .unwrap_or(i64::MAX);
                    const HOUR: i64 = 3_600;
                    const WEEK: i64 = 7 * 86_400;
                    // Only labels younger than a week change over time;
                    // older rows are static dates, so don't force a repaint.
                    if freshest_age < WEEK {
                        cx.notify();
                    }
                    if freshest_age < HOUR {
                        std::time::Duration::from_secs(1)
                    } else {
                        std::time::Duration::from_secs(60)
                    }
                });
                match next {
                    Ok(d) => delay = d,
                    // Shell entity dropped (window closed) — stop ticking.
                    Err(_) => break,
                }
            }
        })
        .detach();

        let initial_tab = Shell::build_tab(
            process.clone(),
            start.clone(),
            start_id,
            focus_handle.clone(),
            window,
            cx,
        );
        // gpui-component's AppMenuBar is the Win/Linux equivalent of
        // macOS's NSApp menu. Reads from the same `cx.set_menus()`
        // global state, so the menu spec lives once in
        // `install_app_menus`. None on macOS — the global menu bar
        // covers it natively.
        let menu_bar = if cfg!(target_os = "macos") {
            None
        } else {
            Some(gpui_component::menu::AppMenuBar::new(cx))
        };
        let mut shell = Self {
            process: process.clone(),
            tabs: vec![initial_tab],
            active: 0,
            focus_handle,
            show_hidden,
            context_row: None,
            context_target: None,
            spring_load: None,
            tree_spring: None,
            favorites_context_path: None,
            breadcrumb_menu_open: false,
            breadcrumb_children: HashMap::new(),
            task_panel_open: false,
            simulated_progress: None,
            simulated_stats: false,
            breadcrumb_editing: false,
            breadcrumb_input,
            shortcuts_help_filter: None,
            shortcuts_help_input,
            // Default off: the preview pane eats ~250-300px on the
            // right and pushes file-list columns (Description in
            // particular) out of view at the default window size.
            // Cmd+P (or whatever shortcut binds the toggle in keymap)
            // brings it back. Once DB-persistence for layout state
            // wires into Shell::new the user's last choice will
            // override this default — until then this is the boot
            // state on every launch.
            preview_visible: false,
            preview_override: None,
            preview_panel: None,
            preview_scroll: ScrollHandle::new(),
            preview_text_scroll: ScrollHandle::new(),
            tab_scroll: ScrollHandle::new(),
            preview_scroll_path: None,
            ui_scale,
            splitter_state: cx.new(|_| crate::splitter::ResizableState::default()),
            sidebar_width: persisted
                .sidebar_width
                .unwrap_or(220.0)
                .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH),
            preview_width: persisted
                .preview_width
                .unwrap_or(380.0)
                .clamp(PREVIEW_MIN_WIDTH, PREVIEW_MAX_WIDTH),
            preview_thumb_h: persisted
                .preview_thumb_height
                .unwrap_or(200.0)
                .clamp(PREVIEW_THUMB_MIN_H, PREVIEW_THUMB_MAX_H),
            preview_thumb_drag: None,
            splitter_save_scheduled: false,
            sidebar_collapsed: persisted.sidebar_collapsed.unwrap_or(false),
            favorites_section_collapsed: false,
            focused_favorite: None,
            favorites_focus: cx.focus_handle(),
            fav_appear: HashSet::new(),
            fav_pulse: HashMap::new(),
            fav_removing: HashSet::new(),
            menu_bar,
            expanded: HashSet::new(),
            tree_children: HashMap::new(),
            dock: None,
            dock_ns_view: 0,
            dock_poll_epoch: 0,
            last_window_title: String::new(),
            was_window_active: true,
            typeahead: None,
            icon_size_track: Bounds::default(),
            icon_size_dragging: false,
            similar_structure_track: Bounds::default(),
            similar_detail_track: Bounds::default(),
            similar_criteria_dragging: None,
            _subscriptions: vec![breadcrumb_subscription, shortcuts_help_subscription],
        };
        shell.process.register_shell(cx.weak_entity());
        // Seed the theme base font size from the persisted/CLI zoom so
        // the very first `Root::render` lays out at the right rem size.
        shell.apply_ui_zoom(cx);
        // §5.3 live-sync: every folder-rendering view observes the
        // Favorites entity through Shell, so a single `cx.notify()`
        // here re-renders the sidebar (FavoritesSection), the
        // breadcrumb (star indicator), and the title-bar header.
        // The file list reads its own delegate's `is_favorited`
        // parallel vec, which `load_path` recomputes from the same
        // entity, so it picks up the change on the next load — for
        // truly synchronous list updates we also push a refresh here.
        let fav_subscription = cx.observe(&shell.process.favorites(), |this, _favs, cx| {
            this.refresh_file_list_favorited(cx);
            // Keep the watcher registered on each favorite's parent dir so
            // a delete/move of a favorited path flips it to Missing live
            // (§8). Idempotent; prune_watches keeps these across nav.
            this.process.watch_favorite_dirs(cx);
            cx.notify();
        });
        shell._subscriptions.push(fav_subscription);

        // §2.2 add/dedup animation signals. The observe above repaints;
        // this captures *which* favorite changed so the section can play
        // the fade-in (Added) or the dedup pulse (DedupPulse). Hydrate
        // emits `Reordered`, not `Added`, so launch never animates.
        let fav_anim_subscription = cx.subscribe(
            &shell.process.favorites(),
            |this, _favs, event: &crate::favorites::FavoritesEvent, cx| {
                use crate::favorites::FavoritesEvent;
                // Each one-shot signal is cleared after its animation
                // window so the row drops back to a plain element (the
                // animation already held its end-state; this just stops
                // it lingering wrapped in an animation div).
                match event {
                    FavoritesEvent::Added { id, .. } => {
                        let id = *id;
                        this.fav_appear.insert(id);
                        cx.spawn(async move |this, cx| {
                            cx.background_executor()
                                .timer(Duration::from_millis(260))
                                .await;
                            let _ = this.update(cx, |this, cx| {
                                if this.fav_appear.remove(&id) {
                                    cx.notify();
                                }
                            });
                        })
                        .detach();
                    }
                    FavoritesEvent::DedupPulse(id) => {
                        let id = *id;
                        *this.fav_pulse.entry(id).or_insert(0) += 1;
                        cx.spawn(async move |this, cx| {
                            cx.background_executor()
                                .timer(Duration::from_millis(560))
                                .await;
                            let _ = this.update(cx, |this, cx| {
                                if this.fav_pulse.remove(&id).is_some() {
                                    cx.notify();
                                }
                            });
                        })
                        .detach();
                    }
                    FavoritesEvent::Removed(fav) => {
                        this.fav_appear.remove(&fav.id);
                        this.fav_pulse.remove(&fav.id);
                        this.fav_removing.remove(&fav.id);
                    }
                    _ => {}
                }
                cx.notify();
            },
        );
        shell._subscriptions.push(fav_anim_subscription);

        // Live thumbnail toggle (Settings window → file list). Render
        // already reflects the global, but the viewport's thumbnails
        // still need warming — `visible_rows_changed` only fires on
        // scroll — so kick a warm of the active tab's current visible
        // range whenever the toggle flips. Turning thumbnails on then
        // fills the visible rows at once instead of waiting for a
        // scroll.
        let thumb_subscription =
            cx.observe_global::<crate::thumbnails::ShowThumbnails>(|this, cx| {
                this.warm_active_visible_thumbnails(cx);
                cx.notify();
            });
        shell._subscriptions.push(thumb_subscription);

        // Live folder-sizing toggle (Settings → Performance). Turning it
        // on computes sizes for every open tab's current listing without a
        // relaunch; turning it off stops any in-flight walks so the change
        // takes effect immediately.
        let folder_size_subscription = cx
            .observe_global::<crate::folder_sizes::FolderSizingEnabled>(|this, cx| {
                if crate::folder_sizes::folder_sizing_enabled(cx) {
                    this.restart_folder_size_passes(false, cx);
                } else {
                    for idx in 0..this.tabs.len() {
                        if let Some(cancel) = this.tabs[idx].folder_size_cancel.take() {
                            cancel.store(true, Ordering::Relaxed);
                        }
                    }
                }
                cx.notify();
            });
        shell._subscriptions.push(folder_size_subscription);

        // Live file-detail-scan toggle (Settings → Performance). Turning
        // it on reloads each open tab in place — a same-path reload
        // re-streams without flicker and re-fires the viewport format-sniff +
        // tag passes under the new setting; turning it off cancels any
        // in-flight viewport worker so scanning stops at once.
        let scan_subscription = cx.observe_global::<crate::prefetch::FileDetailScan>(|this, cx| {
            if crate::prefetch::file_detail_scan_enabled(cx) {
                let targets: Vec<(TabId, PathBuf)> = this
                    .tabs
                    .iter()
                    .filter(|t| t.tool_result.is_none())
                    .map(|t| (t.id, t.current_dir.clone()))
                    .collect();
                for (id, path) in targets {
                    this.load_path_for_tab(id, path, cx);
                }
            } else {
                for tab in &this.tabs {
                    tab.table.update(cx, |state, _cx| {
                        state.delegate_mut().cancel_visible_details();
                    });
                }
            }
            cx.notify();
        });
        shell._subscriptions.push(scan_subscription);

        // Esc cancels an in-progress drag. gpui only auto-cancels on
        // mouse-up, and an element `on_key_down` needs the shell focused
        // (which it may not be mid-drag), so observe keystrokes globally
        // — this fires regardless of focus. Two phases: while the drag is
        // in-window it is gpui state (`active_drag`); once the pointer
        // leaves the window gpui hands it to a native platform drag
        // (`has_active_drag` normally goes false). AppKit cancellation goes
        // through the platform shell; Windows OLE handles Escape itself.
        // Archive promises retain both
        // halves for cross-window drops, so Escape cancels both below
        // (docs/GPUI-UPSTREAM.md #10 and #11).
        let drag_esc_subscription = cx.observe_keystrokes(move |this, e, window, cx| {
            if e.keystroke.key != "escape" {
                return;
            }
            #[cfg(target_os = "macos")]
            let native_drag_active = crate::platform_shell::native_drag_session_active();
            if cx.has_active_drag() {
                cx.stop_active_drag(window);
                this.spring_load = None;
                this.tree_spring = None;
                cx.notify();
            }
            // Archive file promises deliberately keep their in-process
            // ArchiveEntryDrag alive for Ferail-to-Ferail drops. Esc must
            // therefore cancel both halves of the gesture, not choose one.
            #[cfg(target_os = "macos")]
            if native_drag_active {
                crate::platform_shell::cancel_native_drag();
                this.spring_load = None;
                this.tree_spring = None;
            }
        });
        shell._subscriptions.push(drag_esc_subscription);

        // Activation re-seed (docs/features/FRESHNESS.md): when the
        // window returns from the background, re-run the folder-size
        // pass so rows whose TTL lapsed while we were away get picked
        // up. This runs **cache-first** (`force = false`) on purpose:
        // switching apps is something a user does constantly, and a
        // cache-bypassing re-walk here meant every single return
        // re-measured every visible tree from scratch — the size column
        // could never settle. Deep external changes are bounded by the
        // TTL and caught exactly by watcher-driven ancestor
        // invalidation; a user who wants them *now* hits Refresh.
        //
        // The initial launch activation is skipped (`was_window_active`
        // starts `true`), and a same-state re-fire can't pass the
        // transition guard, so app-switch thrash is bounded.
        let activation_subscription = cx.observe_window_activation(window, |this, window, cx| {
            let active = window.is_window_active();
            let returned = active && !this.was_window_active;
            this.was_window_active = active;
            if returned {
                this.restart_folder_size_passes(false, cx);
            }
        });
        shell._subscriptions.push(activation_subscription);

        // Safe mode: never open the metadata SQLite DB — a damaged disk
        // or a DB on slow media is a real hang candidate. Favorites, Ant
        // Trail and Recents stay cold for the session (expected).
        if !crate::safe_mode::enabled() {
            shell.start_metadata_load(cx);
        }
        if start_needs_resolution {
            shell.resolve_start_path_then_load(start, cx);
        } else {
            shell.load_path(start, cx);
        }
        shell
    }

    /// Second half of the window boot (see the `start` comment in
    /// [`Shell::new`]): validate + canonicalize the persisted start path
    /// on the background executor, then load. Until the answer lands the
    /// tab shows its breadcrumbs over an empty list with an "Opening …"
    /// task in the status bar — for a local disk that is a few
    /// milliseconds; for a sleeping drive it is however long the spin-up
    /// takes, with the window responsive the whole time.
    ///
    /// Staleness: if the tab navigated elsewhere before the answer (the
    /// screenshot harness' `--navigate`, a quick Cmd+L) or was closed,
    /// the result is dropped. When the canonical spelling differs from
    /// the raw one (symlink, vanished → home), the tab's seed history
    /// entry is retargeted too, so Back can't land on the raw spelling.
    fn resolve_start_path_then_load(&mut self, raw: PathBuf, cx: &mut Context<Self>) {
        let tab_id = self.active_tab().id;
        let generation = self.active_tab().load_generation;
        let task = self.process.tasks.borrow_mut().begin(
            TaskKind::Enumeration,
            tr!(
                "Opening {path}",
                path = middle_truncate_path(&raw.to_string_lossy(), 40)
            )
            .to_string(),
            false,
        );
        let tasks = self.process.tasks.clone();
        let probe = raw.clone();
        cx.spawn(async move |this, cx| {
            let resolved: Option<PathBuf> = cx
                .background_executor()
                .spawn(async move {
                    probe
                        .is_dir()
                        .then(|| path::canonicalize_for_identity(probe))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                tasks.borrow_mut().end(task);
                let Some(tab) = this.tabs.iter_mut().find(|t| t.id == tab_id) else {
                    return;
                };
                if tab.current_dir != raw || tab.load_generation != generation {
                    return;
                }
                let target = resolved.unwrap_or_else(home_dir);
                if target != raw
                    && let Some(entry) = tab.history.get_mut(tab.history_index)
                {
                    entry.path = target.clone();
                }
                this.load_path_for_tab(tab_id, target, cx);
            });
        })
        .detach();
    }

    /// Build a fresh `Tab` with its own `TableState` entity + table-
    /// event subscription. The subscription captures the new tab's
    /// `TabId` so events from inactive tabs (which shouldn't fire,
    /// since only the active tab is rendered, but defence in depth)
    /// are routed only when the tab is currently active.
    ///
    /// Takes `process` by value so it can be called from `Shell::new`
    /// before the `Shell` struct exists. Other callers can wrap with
    /// `self.make_tab(...)` which forwards `self.process.clone()`.
    pub fn build_tab(
        process: Rc<crate::process_state::ProcessState>,
        at: PathBuf,
        node_id: NodeId,
        shell_focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Tab {
        let tab_id = process.mint_tab_id();
        let delegate = FileListDelegate::new(
            process.fs.clone(),
            process.icons.clone(),
            process.thumbnails.clone(),
            process.tasks.clone(),
            process.cut_marker.clone(),
            process.list_sort.clone(),
            shell_focus,
        );
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .col_selectable(false)
                .col_movable(true)
                .col_resizable(true)
        });
        // Bridge table events on this tab to the Shell's selection
        // state. The closure captures `tab_id` so events from a
        // background tab (rare; only the active tab is hit-tested)
        // never apply gestures meant for the active tab.
        let subscription = cx.subscribe_in(
            &table,
            window,
            move |this, table, event: &TableEvent, window, cx| {
                if this.active_tab().id != tab_id {
                    return;
                }
                match event {
                    TableEvent::RowClicked {
                        row_ix, modifiers, ..
                    } => {
                        this.apply_row_click_gesture(*row_ix, *modifiers, cx);
                    }
                    // Column layout is a process-wide preference: a
                    // reorder or resize on any tab writes through and
                    // seeds every NEW tab/window (existing tabs keep
                    // their live state). app_state::save is cached +
                    // write-behind — no I/O on this thread.
                    TableEvent::MoveColumn(..) | TableEvent::ColumnWidthsChanged(..) => {
                        let spec = table.update(cx, |state, _| {
                            // Fold the live (just-dragged / just-moved) widths
                            // back into the delegate's own column set. The table
                            // rebuilds its column groups from THESE delegate
                            // widths on every `refresh()` — including the
                            // refreshes a background folder-size / prefetch batch
                            // triggers — while a drag only updates the transient
                            // col_groups. Without this write-back an in-flight
                            // worker snaps a resized (or reordered) column
                            // straight back to its construction width, and the
                            // change only "sticks" once the workers stop. Index-
                            // aligned: delegate.columns[ix] ↔ col_widths()[ix]
                            // (a move reorders both), the invariant columns_spec
                            // already relies on.
                            let widths = state.col_widths();
                            let delegate = state.delegate_mut();
                            for (col, w) in delegate.columns.iter_mut().zip(widths.iter()) {
                                col.width = *w;
                            }
                            crate::file_list::columns_spec(
                                &delegate.columns,
                                &delegate.hidden_columns,
                                None,
                            )
                        });
                        let mut s = app_state::load();
                        s.list_columns = Some(spec);
                        app_state::save(&s);
                    }
                    TableEvent::ExternalDrop { row_ix, paths } => {
                        // Dropped onto a folder row — transfer into
                        // that folder (dnd-spec §3.5). Non-folder rows
                        // never emit this; the pane background target
                        // covers them with current-dir semantics. Shared
                        // with the icon grid's folder-cell drop.
                        this.drop_onto_folder_row(*row_ix, paths.clone(), window, cx);
                    }
                    TableEvent::ArchiveAddFromArchive {
                        row_ix,
                        archive,
                        entries,
                        password,
                    } => {
                        // Members dropped on an archive row — add them there
                        // rather than extracting into the current folder.
                        if let Some(target) = this.path_for_row(*row_ix, cx) {
                            this.add_archive_entries_to_archive(
                                archive.clone(),
                                entries.clone(),
                                password.clone(),
                                target,
                                window,
                                cx,
                            );
                        }
                    }
                    TableEvent::ArchiveAddDrop { row_ix, paths } => {
                        // Dropped onto an archive file row — add to it
                        // instead of transferring into a folder.
                        this.drop_onto_archive_row(*row_ix, paths.clone(), window, cx);
                    }
                    TableEvent::ArchiveDrop {
                        row_ix,
                        archive,
                        entries,
                        password,
                    } => {
                        // Extract the dragged entries into the folder they
                        // were dropped on.
                        if let Some(dest) = this.path_for_row(*row_ix, cx) {
                            this.extract_archive_entries_into(
                                archive.clone(),
                                entries.clone(),
                                dest,
                                password.clone(),
                                window,
                                cx,
                            );
                        }
                    }
                    TableEvent::DragHover { row_ix } => {
                        // Spring-load: after a short dwell over a folder
                        // row, drill into it so the user can drop deeper
                        // without releasing the drag. Shared with the
                        // icon grid's folder-cell hover.
                        this.spring_load_hover(*row_ix, cx);
                    }
                    TableEvent::LeadMoved { row_ix, modifiers } => {
                        this.apply_row_keyboard_gesture(*row_ix, *modifiers, cx);
                    }
                    TableEvent::DoubleClickedRow(row_ix) => {
                        this.activate_row(*row_ix, cx);
                    }
                    TableEvent::RightClickedRow(row_ix) => {
                        if let Some(r) = *row_ix {
                            let row_was_selected = this
                                .node_id_at_row(r, cx)
                                .map(|id| this.active_tab().is_selected(id))
                                .unwrap_or(false);
                            this.apply_row_right_click(r, cx);
                            // Spec §2.4: if the user right-clicks
                            // inside the current selection, the
                            // menu targets the whole set. Only
                            // stash a row-specific context target
                            // when the click replaced selection
                            // to that unselected row.
                            this.context_row = if row_was_selected { None } else { Some(r) };
                        } else {
                            this.context_row = None;
                        }
                    }
                    TableEvent::NativeContextMenuRequested(_) => {
                        this.on_show_windows_context_menu(&ShowWindowsContextMenu, window, cx);
                    }
                    TableEvent::RightClickedBackground => {
                        // Empty-space right-click: the background menu's
                        // folder verbs (Get Info / Reveal / Copy Path /
                        // Open Terminal) act on `context_target`, staged
                        // here to the folder being browsed — same pattern
                        // as the breadcrumb menu. Emitted at menu-build
                        // time, so this lands before any item is clicked.
                        this.context_row = None;
                        this.context_target = Some(this.active_tab().current_dir.clone());
                    }
                    _ => {}
                }
            },
        );
        // Filter input — per-tab so cursor / focus / value persist
        // when the user switches tabs. The closure captures `tab_id`
        // so only this tab's enumeration is re-triggered.
        let filter_input = cx.new(|cx| {
            // The return symbol U+23CE has no glyph in the AROS font stack
            // (renders as a tofu box); use a plain word there.
            #[cfg(target_os = "aros")]
            let placeholder = tr!("Filter \u{2026}  Enter to search subfolders");
            #[cfg(not(target_os = "aros"))]
            let placeholder = tr!("Filter \u{2026}  \u{23CE} to search subfolders");
            let mut state = InputState::new(window, cx).placeholder(placeholder);
            // Token autocomplete (`size:`, `mod:`, `locked:`, …) — a
            // static-table lookup, no I/O (filter_complete.rs).
            state.lsp.completion_provider = Some(std::rc::Rc::new(
                crate::filter_complete::FilterCompletionProvider,
            ));
            state
        });
        // Read through the `state` parameter — a captured strong clone of the
        // subscribed entity is a self-cycle that leaks the input past quit
        // (GPUI leaked-handle assertion, seen on Windows 0.6.5).
        let filter_subscription = cx.subscribe_in(
            &filter_input,
            window,
            move |this, state, ev: &InputEvent, window, cx| {
                match ev {
                    InputEvent::Change => {
                        let value = state.read(cx).value().to_string();
                        if let Some(idx) = this.tabs.iter().position(|t| t.id == tab_id) {
                            this.tabs[idx].filter_text = value.clone();
                            // Flat is an explicit recursive snapshot. Typing
                            // must not destroy it or launch another million-row
                            // walk per keystroke; Enter remains the deliberate
                            // escalation to subtree search.
                            if this.tabs[idx]
                                .tool_result
                                .as_ref()
                                .is_some_and(|surface| surface.flat_mode().is_some())
                            {
                                let table = this.tabs[idx].table.clone();
                                table.update(cx, |state, cx| {
                                    state.delegate_mut().apply_flat_filter(&value);
                                    state.refresh(cx);
                                });
                                this.refresh_file_list_selection_in_tab(idx, cx);
                                cx.notify();
                                return;
                            }
                            // Editing the filter while showing a results
                            // view returns to the live directory, then
                            // applies the in-directory filter.
                            this.tabs[idx].tool_result = None;
                            let path = this.tabs[idx].current_dir.clone();
                            this.load_path_for_tab(tab_id, path, cx);
                        }
                    }
                    // Enter escalates the in-directory filter into a
                    // recursive / global search of the current folder
                    // and below (docs/features/SEARCH.md).
                    InputEvent::PressEnter { .. } => {
                        let value = state.read(cx).value().to_string();
                        this.start_subtree_search(tab_id, value, Some(window.window_handle()), cx);
                    }
                    _ => {}
                }
            },
        );
        Tab::new_internal(
            tab_id,
            at,
            node_id,
            table,
            subscription,
            filter_input,
            filter_subscription,
            crate::grid::ViewMode::persisted_default(),
            cx.focus_handle(),
        )
    }

    /// `build_tab` wrapper for callers that already have `&mut self`.
    pub fn make_tab(
        &mut self,
        at: PathBuf,
        node_id: NodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Tab {
        Self::build_tab(
            self.process.clone(),
            at,
            node_id,
            self.focus_handle.clone(),
            window,
            cx,
        )
    }

    /// "Which row is this action targeting?" — context_row first
    /// (right-click triggered), then selected (keyboard / single-
    /// click). Consumes context_row so the next keyboard action uses
    /// the keyboard selection.
    ///
    /// After the multi-select rework, the keyboard fallback is the
    /// **lead's row index in the current model** — the row index
    /// derived from `Tab::lead` against the delegate's `entries`.
    /// The lead is the right semantic target: a single-row action
    /// like Rename or Compress on a multi-selection should operate
    /// on the focused row, the same way Finder does.
    fn target_row(&mut self, cx: &App) -> Option<usize> {
        if let Some(r) = self.context_row.take() {
            Some(r)
        } else {
            self.active_tab()
                .lead_row(&self.active_tab().table.read(cx).delegate().entries)
        }
    }

    /// Resolve a row to an absolute path on disk. Reuses the same
    /// id_for_path fallback that activate_row uses.
    /// Kind of the entry at `row_ix` in the active tab (cached read).
    pub fn entry_kind_at_row(&self, row_ix: usize, cx: &App) -> Option<EntryKind> {
        self.active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .get(row_ix)
            .map(|e| e.kind)
    }

    /// Schedule the preview providers for `row_ix` — unless it's a
    /// folder. Folders have no file preview (qlmanage on a directory
    /// is wasted work and the text read just fails), so the request is
    /// skipped and the pane shows folder metadata only.
    pub(crate) fn request_preview_for_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        if matches!(
            self.entry_kind_at_row(row_ix, cx),
            Some(EntryKind::Directory)
        ) {
            return;
        }
        if let Some(p) = self.path_for_row(row_ix, cx) {
            crate::preview::request(self, p, cx);
        }
    }

    pub fn path_for_row(&self, row_ix: usize, cx: &App) -> Option<PathBuf> {
        let entry = self
            .active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .get(row_ix)?
            .clone();
        self.process
            .node_store
            .borrow_mut()
            .path_snapshot_for_job(entry.id, "Shell::path_for_row")
            .or_else(|| {
                self.active_tab()
                    .table
                    .read(cx)
                    .delegate()
                    .path_for_entry(entry.id)
            })
            .or_else(|| {
                let mut p = self.active_tab().current_dir.clone();
                p.push(entry.name.as_ref());
                Some(p)
            })
    }

    fn entry_path_for_row(&self, row_ix: usize, cx: &App) -> Option<(usize, FileEntry, PathBuf)> {
        let entry = self
            .active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .get(row_ix)?
            .clone();
        let path = self.path_for_row(row_ix, cx)?;
        Some((row_ix, entry, path))
    }

    /// Visible-order selection snapshot for bulk commands and drag
    /// payloads. This is intentionally model/cache-only: it reads the
    /// active tab's NodeId set plus the delegate's current rows and
    /// path cache, never the filesystem.
    fn selected_entries_visible_order(&self, cx: &App) -> Vec<(usize, FileEntry, PathBuf)> {
        let tab = self.active_tab();
        let visible_count = tab.table.read(cx).delegate().entries.len();
        if tab.selection_is_empty(visible_count) {
            return Vec::new();
        }
        self.active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .iter()
            .enumerate()
            .filter_map(|(row_ix, entry)| {
                tab.is_selected(entry.id)
                    .then(|| self.entry_path_for_row(row_ix, cx))
                    .flatten()
            })
            .collect()
    }

    /// Resolve the target set for a command from a given `context_row`.
    /// A right-click on an unselected row sets `context_row` to just that
    /// row; a right-click inside a selected set leaves it empty, so
    /// bulk-capable commands operate on the whole visible selection.
    /// Keyboard/menu invocations pass `None` and use the selection when
    /// present, falling back to the lead row for single-row commands.
    ///
    /// Read-only (`&self`): the caller decides whether to consume
    /// `context_row`. `action_entries_visible_order` takes it (one-shot
    /// dispatch). Its menu-side twin is `file_list::resolve_menu_targets`,
    /// which resolves the same set from the clicked row + selection when
    /// the menu builds.
    fn resolve_targets(
        &self,
        context_row: Option<usize>,
        cx: &App,
    ) -> Vec<(usize, FileEntry, PathBuf)> {
        if let Some(row) = context_row {
            return self.entry_path_for_row(row, cx).into_iter().collect();
        }
        let selected = self.selected_entries_visible_order(cx);
        if !selected.is_empty() {
            return selected;
        }
        self.active_tab()
            .lead_row(&self.active_tab().table.read(cx).delegate().entries)
            .and_then(|row| self.entry_path_for_row(row, cx))
            .into_iter()
            .collect()
    }

    /// Resolve and consume `context_row`: the set a dispatched command
    /// acts on. Consuming means the next keyboard action falls through to
    /// the selection/lead instead of re-targeting the last right-click.
    fn action_entries_visible_order(&mut self, cx: &App) -> Vec<(usize, FileEntry, PathBuf)> {
        let context_row = self.context_row.take();
        self.resolve_targets(context_row, cx)
    }

    /// Number of rows the next action will resolve, without materializing
    /// their entries or paths. Used to keep system APIs with inherently
    /// eager payloads away from multi-million-row allocations.
    fn action_target_count(&self, cx: &App) -> usize {
        if self.context_row.is_some() {
            return 1;
        }
        let tab = self.active_tab();
        let visible = tab.table.read(cx).delegate().entries.len();
        let selected = tab.selection_count(visible);
        if selected > 0 {
            selected
        } else {
            usize::from(tab.lead.is_some())
        }
    }

    fn on_navigate_back(&mut self, _: &NavigateBack, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate_back(cx);
    }

    fn on_navigate_forward(&mut self, _: &NavigateForward, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate_forward(cx);
    }

    fn on_open_selected(&mut self, _: &OpenSelected, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.active_tab().platform_namespace.as_ref() {
            if let Some(item_id) = session.selection().lead() {
                let tab_id = self.active_tab().id;
                self.click_platform_item(tab_id, item_id, false, true, cx);
            }
            return;
        }
        let entries = self.action_entries_visible_order(cx);
        if entries.is_empty() {
            return;
        }
        if entries.len() == 1 {
            self.activate_row(entries[0].0, cx);
            return;
        }
        // FanOut: opening N items launches N apps / opens N folder tabs.
        // Power users can open many at once; `confirm_fanout` only asks
        // once the count crosses the threshold (then opens all).
        let count = entries.len();
        self.confirm_fanout(
            count,
            tr!("Open Items?"),
            trn!("Open {n} item?", "Open {n} items?", count).to_string(),
            tr!("Open"),
            window,
            cx,
            move |this, window, cx| {
                let mut files = Vec::new();
                for (_, entry, path) in entries {
                    if matches!(entry.kind, EntryKind::Directory) {
                        this.open_path_in_new_tab(path, window, cx);
                    } else {
                        files.push(path);
                    }
                }
                if !files.is_empty() {
                    cx.background_spawn(async move {
                        for path in files {
                            let _ = crate::platform_shell::open_with_default(&path);
                        }
                    })
                    .detach();
                }
            },
        );
    }

    fn on_refresh(&mut self, _: &Refresh, window: &mut Window, cx: &mut Context<Self>) {
        // The same explicit gesture also refreshes the small cached platform
        // root list. This remains entirely off-thread and is never triggered
        // by rendering; Safe Mode deliberately suppresses platform probes.
        if !crate::safe_mode::enabled() {
            crate::platform_locations::refresh(cx);
        }
        if self.active_tab().platform_namespace.is_some() {
            let tab_id = self.active_tab().id;
            let request = self
                .active_tab_mut()
                .platform_namespace
                .as_mut()
                .map(|session| session.refresh());
            if let Some((request, cancel)) = request {
                self.start_platform_listing(tab_id, request, cancel, cx);
            }
            return;
        }
        if self
            .active_tab()
            .tool_result
            .as_ref()
            .is_some_and(|surface| surface.flat_mode().is_some())
        {
            let tab_id = self.active_tab().id;
            self.restart_flat_view(tab_id, Some(window.window_handle()), cx);
            return;
        }
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
        // Re-read the directory *and* re-derive everything cached about
        // it from disk: magic/description (bypassing the metadata-DB
        // cache) and the recursive folder sizes (bypassing the
        // `folder_sizes` cache). Refresh is the user's explicit "measure
        // this again", and the only gesture that pays for a full
        // re-walk — see docs/features/FRESHNESS.md.
        //
        // `load_path_for_tab` just reset both flags to `false` and
        // bumped the load generation, so arming them here scopes the
        // forced work to exactly this load: a load superseded before it
        // finishes is dropped by the generation guard, and the
        // superseding load reset the flags.
        let tab_id = self.active_tab().id;
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.force_resniff = true;
            tab.force_folder_sizes = true;
        }
    }

    /// Reveal the desktop via the Dock's private Show Desktop path. The
    /// button/menu entry that dispatches this are only shown when the
    /// symbol resolved, but we still treat a failed call as a silent
    /// no-op so nothing can crash the UI.
    fn on_show_desktop(&mut self, _: &ShowDesktop, _: &mut Window, _cx: &mut Context<Self>) {
        let _ = crate::platform_shell::show_desktop();
    }

    // -----------------------------------------------------------------
    // Window docking (docs/features/DOCK.md).
    //
    // Dock the whole window to a screen edge as an auto-hiding drawer: it
    // floats over everything (NSFloatingWindowLevel + join-all-spaces) and
    // slides off-screen leaving a thin handle; slamming the cursor into that
    // screen edge slides it back. All the geometry is pure in `dock.rs`; this
    // is the GPUI glue — capture the window/screen frames once at dock time,
    // then drive a self-re-arming poll that reads the cursor and moves the
    // window each tick.
    // -----------------------------------------------------------------

    /// gpui's window content `NSView`, the handle the native docking calls
    /// reach the `NSWindow` through. `None` off AppKit (the non-mac shell
    /// stubs no-op regardless, so docking is a macOS feature in practice).
    fn window_ns_view(window: &Window) -> Option<*mut std::ffi::c_void> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        // UFCS: gpui::Window has an inherent `window_handle()` (the entity
        // handle) that shadows the raw-window-handle trait method.
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::AppKit(h) => Some(h.ns_view.as_ptr()),
            _ => None,
        }
    }

    fn on_dock_left(&mut self, _: &DockLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.set_dock(Some(DockEdge::Left), window, cx);
    }
    fn on_dock_right(&mut self, _: &DockRight, window: &mut Window, cx: &mut Context<Self>) {
        self.set_dock(Some(DockEdge::Right), window, cx);
    }
    fn on_undock(&mut self, _: &Undock, window: &mut Window, cx: &mut Context<Self>) {
        self.set_dock(None, window, cx);
    }

    /// Enter, switch, or leave the docked drawer. `None` undocks (restoring the
    /// pre-dock frame and dropping the floating / all-spaces behaviors);
    /// `Some(edge)` docks to that edge, snapping into the docked position fully
    /// shown so the following slide reads as "snap to edge → tuck away".
    pub fn set_dock(
        &mut self,
        edge: Option<DockEdge>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ns_view) = Self::window_ns_view(window) else {
            return; // non-AppKit: docking is a macOS feature
        };

        match edge {
            None => {
                if let Some(d) = self.dock.take() {
                    // The display may have changed while docked — clamp
                    // the remembered frame onto the CURRENT screen so
                    // undock can't strand the window off every display.
                    let mut r = d.restore;
                    if let Some((sx, sy, sw, sh)) =
                        crate::platform_shell::screen_visible_frame_for_window(ns_view)
                    {
                        r.w = r.w.min(sw);
                        r.h = r.h.min(sh);
                        r.x = r.x.clamp(sx, sx + sw - r.w);
                        r.y = r.y.clamp(sy, sy + sh - r.h);
                    }
                    crate::platform_shell::set_window_frame(ns_view, r.x, r.y, r.w, r.h);
                    crate::platform_shell::set_window_all_spaces(ns_view, false);
                    crate::platform_shell::set_window_floating(ns_view, false);
                    // Bumping the epoch makes the running poll exit next tick.
                    self.dock_poll_epoch = self.dock_poll_epoch.wrapping_add(1);
                    self.dock_ns_view = 0;
                    cx.notify();
                }
            }
            Some(e) => {
                // AppKit handles setFrame: on a native-fullscreen window
                // poorly (Space bookkeeping); refuse instead of glitching.
                if crate::platform_shell::window_is_fullscreen(ns_view) {
                    window.push_notification(
                        gpui_component::notification::Notification::info(tr!(
                            "Exit full screen before docking the window."
                        )),
                        cx,
                    );
                    return;
                }
                let Some(screen) = crate::platform_shell::screen_visible_frame_for_window(ns_view)
                    .map(|(x, y, w, h)| ScreenFrame::new(x, y, w, h))
                else {
                    return;
                };
                // Keep the original pre-dock frame across edge switches.
                let restore = self
                    .dock
                    .as_ref()
                    .map(|d| d.restore)
                    .or_else(|| {
                        crate::platform_shell::window_frame(ns_view)
                            .map(|(x, y, w, h)| ScreenFrame::new(x, y, w, h))
                    })
                    .unwrap_or(screen);
                // The drawer keeps the window's OWN size and vertical
                // position — docking is a pure horizontal translation.
                // (gpui's drawable doesn't follow an out-of-band AppKit
                // resize; stretching the window to the screen height
                // left the extra area black.)
                let win = crate::platform_shell::window_frame(ns_view)
                    .map(|(x, y, w, h)| ScreenFrame::new(x, y, w, h))
                    .unwrap_or(restore);
                let mut state = DockState::new(e, screen, win, restore);
                // Start fully shown, then let the poll tuck it away unless the
                // cursor is already at the edge.
                state.revealed = false;
                state.progress = 1.0;
                let shown = dock::revealed_frame(e, screen, win);

                crate::platform_shell::set_window_floating(ns_view, true);
                crate::platform_shell::set_window_all_spaces(ns_view, true);
                crate::platform_shell::set_window_frame(
                    ns_view, shown.x, shown.y, shown.w, shown.h,
                );

                self.dock = Some(state);
                self.dock_ns_view = ns_view as usize;
                self.start_dock_poll(cx);
                cx.notify();
            }
        }
    }

    /// (Re)start the reveal poll: bump the epoch (retiring any prior loop) and
    /// arm the first tick.
    fn start_dock_poll(&mut self, cx: &mut Context<Self>) {
        let epoch = self.dock_poll_epoch.wrapping_add(1);
        self.dock_poll_epoch = epoch;
        Self::schedule_dock_poll(epoch, Duration::from_millis(16), cx);
    }

    /// Arm a single poll tick after `interval`. Re-armed by `dock_poll_tick`
    /// itself (self-re-arming one-shot, like the viewer's slideshow timer) so
    /// no long-lived task handle has to be tracked.
    fn schedule_dock_poll(epoch: u64, interval: Duration, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(interval).await;
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |this, cx| this.dock_poll_tick(epoch, cx));
        })
        .detach();
    }

    /// One reveal-poll tick. Reads the global cursor, flips the drawer's
    /// revealed target on an edge-slam (or while the pointer is still over it),
    /// steps the slide, and moves the window only if it actually advanced — so
    /// a settled drawer touches AppKit zero times per tick. Re-arms itself
    /// unless the loop was superseded (epoch bumped) or the window undocked.
    fn dock_poll_tick(&mut self, epoch: u64, cx: &mut Context<Self>) {
        if self.dock_poll_epoch != epoch {
            return; // superseded by a re-dock / undock
        }
        if self.dock.is_none() {
            return; // undocked
        }
        let ptr = self.dock_ns_view as *mut std::ffi::c_void;
        // Liveness probe for the cached raw pointer: `window_frame`
        // returning None means there's no NSWindow behind the view any
        // more (teardown) — stop the loop instead of msg_send'ing into
        // a dead view.
        let Some(live_frame) = crate::platform_shell::window_frame(ptr) else {
            self.dock = None;
            self.dock_ns_view = 0;
            return;
        };
        let mouse = crate::platform_shell::current_mouse_location();
        let Some(dock) = self.dock.as_mut() else {
            return;
        };
        let wants = dock.wants_reveal(mouse);
        if wants && !dock.revealed {
            // Transitioning hidden → revealing: the display arrangement
            // or the drawer's user-dragged size may have changed while
            // tucked away. Re-query the screen and adopt the actual
            // window frame so the slide targets live geometry instead
            // of the dock-time snapshot (which could be a disconnected
            // display). Size passes through untouched — docking never
            // resizes the window.
            if let Some((sx, sy, sw, sh)) =
                crate::platform_shell::screen_visible_frame_for_window(ptr)
            {
                dock.screen = ScreenFrame::new(sx, sy, sw, sh);
            }
            dock.win = ScreenFrame::new(live_frame.0, live_frame.1, live_frame.2, live_frame.3);
        }
        dock.revealed = wants;
        let frame = dock.step().then(|| dock.current_frame());
        let animating = dock.is_animating();
        if let Some(f) = frame {
            crate::platform_shell::set_window_frame(ptr, f.x, f.y, f.w, f.h);
            // The handle overlay shows/hides as the drawer slides; repaint.
            cx.notify();
        }
        let interval = Duration::from_millis(if animating { 16 } else { 33 });
        Self::schedule_dock_poll(epoch, interval, cx);
    }

    fn on_toggle_hidden(&mut self, _: &ToggleHidden, _: &mut Window, cx: &mut Context<Self>) {
        crate::trail::command("Toggle Hidden Files");
        self.toggle_hidden(cx);
    }

    fn on_toggle_flat_view(
        &mut self,
        _: &ToggleFlatView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Toggle Flat View");
        let tab_id = self.active_tab().id;
        self.toggle_flat_view(tab_id, Some(window.window_handle()), cx);
    }

    fn on_focus_filter(&mut self, _: &FocusFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_filter_input(window, cx);
        // Re-assert the focus after the current update cycle. When this fires
        // from the View > Find menu item (rather than the Ctrl+F key), the app
        // menu bar restores focus to its previously-focused element as it
        // closes — which would otherwise immediately steal focus straight back
        // out of the filter, making the menu item look like it did nothing.
        // Deferring lets our focus win the race; it's a harmless no-op repeat
        // for the direct Ctrl+F path.
        let filter = self.active_tab().filter_input.clone();
        window.defer(cx, move |window, cx| {
            filter.read(cx).focus_handle(cx).focus(window, cx);
        });
    }

    /// Public-from-screenshot-CLI helper: focuses the filter input
    /// (same effect as Cmd+F). Stage 2's `--search` flag uses this.
    pub fn focus_filter_input(&self, window: &mut Window, cx: &mut App) {
        self.active_tab()
            .filter_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
    }

    fn on_clear_filter(&mut self, _: &ClearFilter, window: &mut Window, cx: &mut Context<Self>) {
        // This fires only when the filter field owns focus (Esc in the
        // shell pane routes to ClearSelection instead). When the field
        // has text or the tab is showing a results view, Esc clears the
        // field / leaves the results and reloads the directory — but
        // keeps focus in the field so the user can immediately type a
        // new query (their preference). Only when the field is already
        // empty and not showing results does Esc fall through to the
        // §2.5 selection-clear, which escapes out to the shell pane.
        let has_text = !self.active_tab().filter_text.is_empty();
        let in_results = self.active_tab().tool_result.is_some();
        if has_text || in_results {
            let filter_input = self.active_tab().filter_input.clone();
            filter_input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
            self.active_tab_mut().filter_text.clear();
            self.active_tab_mut().tool_result = None;
            let path = self.active_tab().current_dir.clone();
            self.load_path(path, cx);
            self.focus_filter_input(window, cx);
            return;
        }
        let visible_count = self.active_tab().table.read(cx).delegate().entries.len();
        if !self.active_tab().selection_is_empty(visible_count) {
            self.clear_active_selection(cx);
            self.focus_handle.focus(window, cx);
        }
    }

    /// Cmd+Shift+H — navigate the active tab to the home directory.
    fn on_go_home(&mut self, _: &GoHome, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(home_dir(), cx);
    }

    /// Cmd+G — open the "Go to Folder" prompt.
    pub fn on_go_to_folder(&mut self, _: &GoToFolder, window: &mut Window, cx: &mut Context<Self>) {
        self.open_go_to_folder_prompt(true, window, cx);
    }

    /// The Go to Folder modal: a single path field pre-filled with the
    /// active tab's directory and selected on open, so a paste replaces
    /// it outright while an edit still starts from somewhere real. It
    /// carries the same background folder autocomplete as the Cmd+L
    /// breadcrumb (`crate::path_complete`). Enter or the **Go** button
    /// commits.
    ///
    /// `in_new_tab` is true for the ordinary invocation — the typed
    /// folder lands in a new tab beside the current one, leaving what
    /// the user was looking at intact. The window that `boot` opens
    /// *because* nothing was open passes false: its lone tab is a blank
    /// stand-in, so it navigates in place instead of stacking a second.
    pub fn open_go_to_folder_prompt(
        &mut self,
        in_new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::dialog::DialogFooter;

        crate::trail::command("Go to Folder");
        let current = self.active_tab().current_dir.to_string_lossy().into_owned();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder(tr!("/path/to/folder"));
            state.lsp.completion_provider =
                Some(Rc::new(crate::path_complete::PathCompletionProvider));
            state
        });
        input.update(cx, |state, cx| {
            state.set_value(current, window, cx);
        });
        // One commit path shared by Enter (the dialog's `on_ok`) and the
        // Go button, so the two can't drift.
        let shell = cx.entity();
        let commit = {
            let input = input.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let raw = input.read(cx).value().to_string();
                if raw.trim().is_empty() {
                    // Nothing typed: close without navigating.
                    return;
                }
                shell.update(cx, |this, cx| {
                    this.go_to_pasted_path(raw, in_new_tab, window, cx);
                });
            })
        };
        let dialog_input = input.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let input = dialog_input.clone();
            let commit_enter = commit.clone();
            let commit_click = commit.clone();
            dialog
                .title(tr!("Go to Folder"))
                .child(Input::new(&input).small())
                // A `Dialog` only draws buttons it's given a footer for;
                // `button_props` alone renders nothing. Cancel first,
                // Go primary — Esc and Enter are the keyboard twins.
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("go-to-folder-cancel")
                                .label(tr!("Cancel"))
                                .small()
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("go-to-folder-go")
                                .label(tr!("Go"))
                                .primary()
                                .small()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    commit_click(window, cx);
                                }),
                        ),
                )
                .on_ok(move |_, window, cx: &mut App| {
                    commit_enter(window, cx);
                    true
                })
        });
        // Focus + select-all on the next frame, once the dialog and its
        // input are mounted — doing it synchronously wouldn't stick.
        // Same shape as `open_text_prompt`.
        window.on_next_frame(move |window, cx| {
            input.read(cx).focus_handle(cx).focus(window, cx);
            window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        });
    }

    /// Commit a Go to Folder entry. Resolution — canonicalisation plus
    /// the "is this a file?" probe — is filesystem work, so it runs on
    /// the background executor and only the result comes back to the
    /// UI thread (Prime Directive). A path that names a *file* opens
    /// its enclosing folder, which is what a copied full path from a
    /// terminal or a chat message usually is; a path that doesn't
    /// resolve at all is handed to navigation unchanged so the pane's
    /// existing enumeration error reports it.
    pub fn go_to_pasted_path(
        &mut self,
        raw: String,
        in_new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let typed = path::parse_pasted_path(&raw);
        crate::log_info!(90, "go to folder: {raw:?} -> {}", typed.display());
        let win = window.window_handle();
        let shell = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let target = cx
                .background_executor()
                .spawn(async move { path::resolve_go_to_target(typed) })
                .await;
            let _ = win.update(cx, |_, window, cx| {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                shell.update(cx, |this, cx| {
                    if in_new_tab {
                        this.open_path_in_new_tab(target, window, cx);
                    } else {
                        this.navigate(target, cx);
                    }
                });
            });
        })
        .detach();
    }

    /// Cmd+L: open breadcrumb edit mode. Pre-fills the input with
    /// the active tab's current directory, focuses it, and selects
    /// all text so the user can immediately type a replacement
    /// path.
    pub fn on_edit_breadcrumb(
        &mut self,
        _: &EditBreadcrumb,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.breadcrumb_editing = true;
        let current = self.active_tab().current_dir.to_string_lossy().into_owned();
        self.breadcrumb_input.update(cx, |state, cx| {
            state.set_value(current, window, cx);
        });
        self.breadcrumb_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    /// Cmd+/ (or `--shortcuts-help[-filter]` CLI flag): open the
    /// keyboard-shortcuts help overlay. Filter starts empty
    /// (showing every command in the catalogue, grouped by
    /// category); the user can type to narrow down.
    pub fn on_shortcuts_help(
        &mut self,
        _: &ShortcutsHelp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_shortcuts_help(String::new(), window, cx);
    }

    /// Programmatic version of `on_shortcuts_help` — the CLI flag
    /// can seed the filter so the screenshot captures a focused
    /// subset of the catalogue.
    pub fn open_shortcuts_help(
        &mut self,
        initial_filter: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shortcuts_help_filter = Some(initial_filter.clone());
        self.shortcuts_help_input.update(cx, |state, cx| {
            state.set_value(initial_filter, window, cx);
        });
        self.shortcuts_help_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    /// Dismiss the shortcuts-help overlay (called when the user
    /// clicks the backdrop or presses Esc).
    pub fn close_shortcuts_help(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shortcuts_help_filter = None;
        // Return keyboard focus to the shell. The palette's filter
        // Input held focus while open; without this the window keeps
        // focus on the now-unmounted Input and the next Cmd+K (and
        // every other shortcut) is dropped until the user clicks back
        // into the app.
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Cmd+Shift+D — open the Disk Usage window at the active tab's
    /// current directory. Spawns a new native window; if opening
    /// fails (rare — only when gpui can't allocate a window), the
    /// failure is logged-and-ignored.
    /// Throttled persistence of the splitter pane widths. Called
    /// from the `on_resize` callback (fires per drag tick at 60 Hz).
    /// Writes the config file at most once per
    /// `SPLITTER_PERSIST_INTERVAL`; the final width at drag-end
    /// always lands because subsequent renders re-check the
    /// interval and flush. Trades a few-hundred-millisecond
    /// recoverability against not hammering the file system.
    /// Arm a trailing, debounced write of the current splitter widths.
    /// The first resize tick of a drag schedules a write
    /// `SPLITTER_PERSIST_INTERVAL` later; further ticks within that window
    /// are no-ops (a write is already queued). When the timer fires it
    /// reads the *latest* widths off `self`, so the value at drag-end always
    /// persists — and a single drag costs at most a couple of file writes.
    pub(crate) fn schedule_splitter_save(&mut self, cx: &mut Context<Self>) {
        if self.splitter_save_scheduled {
            return;
        }
        self.splitter_save_scheduled = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(SPLITTER_PERSIST_INTERVAL)
                .await;
            let _ = this.update(cx, |this, _cx| {
                this.splitter_save_scheduled = false;
                let mut state = app_state::load();
                state.sidebar_width = Some(this.sidebar_width);
                state.preview_width = Some(this.preview_width);
                state.preview_thumb_height = Some(this.preview_thumb_h);
                app_state::save(&state);
            });
        })
        .detach();
    }

    /// Open the first selected archive in the embedded workbench view.
    pub fn on_open_archive(
        &mut self,
        _: &OpenAsArchive,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Any file is a candidate: the extension may say nothing (a .docx or
        // .jar is a zip underneath), so the workbench probes the content
        // off-thread and reports plainly if it isn't openable.
        let archive = self
            .action_entries_visible_order(cx)
            .into_iter()
            .find(|(_, entry, _)| !matches!(entry.kind, ferail_core::EntryKind::Directory))
            .map(|(_, _, path)| path);
        if let Some(archive) = archive {
            self.dock_archive_view(archive, window, cx);
        }
    }

    /// Build the archive view for `archive` and dock it as the active tab's
    /// tool-result surface, cancelling the tab's directory-load work (mirrors
    /// `dock_disk_usage_view`). The tab stays rooted at `current_dir`.
    /// The docked archive workbench, when one is open.
    pub fn active_archive_view(&self) -> Option<Entity<crate::archive::ArchiveView>> {
        self.active_tab()
            .tool_result
            .as_ref()
            .and_then(|s| s.archive_mode())
            .map(|a| a.view.clone())
    }

    /// Open a specific archive in the workbench, bypassing the selection.
    /// Used by the screenshot harness; the context action goes through
    /// `on_open_archive`.
    pub fn open_archive_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock_archive_view(path, window, cx);
    }

    fn dock_archive_view(&mut self, archive: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // The workbench owns its own table (so it can be popped out later) and
        // resolves the format itself, off-thread, from extension-or-content.
        let process = self.process.clone();
        let focus = self.focus_handle.clone();
        let shell_weak = cx.weak_entity();
        let view = cx.new(|cx| {
            let mut v =
                crate::archive::ArchiveView::new(archive.clone(), process, focus, window, cx);
            v.set_shell(shell_weak);
            v
        });
        self.dock_archive_entity(archive, view, cx);
    }

    /// Dock an existing archive view into the active tab — used both by a fresh
    /// open and by a standalone window docking itself back.
    pub(crate) fn dock_archive_entity(
        &mut self,
        archive: PathBuf,
        view: Entity<crate::archive::ArchiveView>,
        cx: &mut Context<Self>,
    ) {
        let tab_id = self.active_tab().id;
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        if let Some(cancel) = self.tabs[idx].load_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = self.tabs[idx].folder_size_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = self.tabs[idx].prefetch_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(previous) = self.tabs[idx].load_task.take() {
            self.process.tasks.borrow_mut().end(previous);
        }
        self.tabs[idx].load_generation = self.tabs[idx].load_generation.wrapping_add(1);
        self.tabs[idx].load_staging = None;
        self.tabs[idx].dupe_groups.clear();
        let surface = ToolResultSurface::archive(archive, view);
        surface.handle_host_event(ToolHostEvent::HostChanged(ToolHostContext::Docked), cx);
        self.tabs[idx].tool_result = Some(surface);
        cx.notify();
    }

    fn archive_dock_owner(&self, cx: &mut Context<Self>) -> crate::archive::ArchiveDockOwner {
        let weak: WeakEntity<Self> = cx.weak_entity();
        Rc::new(move |archive, view, cx| {
            if let Some(s) = weak.upgrade() {
                s.update(cx, move |this, cx| {
                    this.dock_archive_entity(archive, view, cx);
                });
            }
        })
    }

    /// Move the docked archive workbench into its own window. Side-by-side with
    /// Finder is what makes dragging files *into* an archive practical.
    pub(super) fn pop_out_active_archive(&mut self, cx: &mut Context<Self>) {
        let Some(tab::ToolResultMode::Archive(am)) = self
            .active_tab()
            .tool_result
            .as_ref()
            .map(|surface| &surface.mode)
        else {
            return;
        };
        let archive = am.archive.clone();
        let view = am.view.clone();
        let dock_owner = self.archive_dock_owner(cx);
        match crate::archive::open_existing_window(
            archive.clone(),
            view.clone(),
            Some(dock_owner),
            cx,
        ) {
            Ok(_) => {
                ToolResultSurface::archive(archive, view)
                    .handle_host_event(ToolHostEvent::HostChanged(ToolHostContext::Windowed), cx);
                self.close_active_tool_result(cx);
            }
            Err(e) => {
                crate::log_warn!(90, "archive: pop-out failed: {e:?}");
            }
        }
    }

    fn on_pop_out_archive(
        &mut self,
        _: &PopOutArchive,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pop_out_active_archive(cx);
    }

    pub fn on_open_disk_usage(
        &mut self,
        _: &OpenDiskUsage,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.active_tab().current_dir.clone();
        self.dock_disk_usage_root(root, cx);
    }

    fn dock_disk_usage_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let fs = self.process.fs.clone();
        let tasks = self.process.tasks.clone();
        let notify_owner = self.disk_usage_notify_owner(cx);
        let shell_weak = cx.weak_entity();
        let view = cx.new(|cx| {
            let mut view = crate::disk_usage::DiskUsageView::new(
                root.clone(),
                fs.clone(),
                tasks.clone(),
                Some(notify_owner.clone()),
                None,
                cx,
            );
            // Lets the DU context menu open Get Info windows and
            // reload affected tabs after a trash.
            view.shell = Some(shell_weak);
            view
        });
        self.dock_disk_usage_view(root, view, cx);
    }

    fn dock_disk_usage_view(
        &mut self,
        root: PathBuf,
        view: Entity<crate::disk_usage::DiskUsageView>,
        cx: &mut Context<Self>,
    ) {
        if self.active_tab().current_dir != root {
            self.navigate(root.clone(), cx);
        }

        let tab_id = self.active_tab().id;
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };

        if let Some(cancel) = self.tabs[idx].load_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = self.tabs[idx].folder_size_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = self.tabs[idx].prefetch_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(previous) = self.tabs[idx].load_task.take() {
            self.process.tasks.borrow_mut().end(previous);
        }
        self.tabs[idx].load_generation = self.tabs[idx].load_generation.wrapping_add(1);
        self.tabs[idx].load_staging = None;
        self.tabs[idx].dupe_groups.clear();

        view.update(cx, |view, cx| view.set_dock_owner(None, cx));
        let surface = ToolResultSurface::disk_usage(root, view);
        surface.handle_host_event(ToolHostEvent::HostChanged(ToolHostContext::Docked), cx);
        self.tabs[idx].tool_result = Some(surface);
        cx.notify();
    }

    fn disk_usage_notify_owner(&self, cx: &mut Context<Self>) -> Rc<dyn Fn(&mut App)> {
        // The DU view owns its own entity, so it can't drive our notify
        // directly. We hand it a callback closing over a weak handle to
        // this Shell; when the scan begins/ends a task, it calls back and
        // we re-render to refresh the status bar.
        let weak: WeakEntity<Self> = cx.weak_entity();
        Rc::new(move |cx| {
            if let Some(s) = weak.upgrade() {
                s.update(cx, |_, cx| cx.notify());
            }
        })
    }

    fn disk_usage_dock_owner(&self, cx: &mut Context<Self>) -> DiskUsageDockOwner {
        let weak: WeakEntity<Self> = cx.weak_entity();
        Rc::new(move |root, view, cx| {
            if let Some(s) = weak.upgrade() {
                s.update(cx, move |this, cx| {
                    this.dock_disk_usage_view(root, view, cx);
                });
            }
        })
    }

    pub(super) fn close_active_tool_result(&mut self, cx: &mut Context<Self>) {
        if self.active_tab().tool_result.is_none() {
            return;
        }
        self.active_tab_mut().tool_result = None;
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
    }

    fn on_close_tool_result(
        &mut self,
        _: &CloseToolResult,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_active_tool_result(cx);
    }

    pub(super) fn pop_out_active_disk_usage(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab::ToolResultMode::DiskUsage(du)) = self
            .active_tab()
            .tool_result
            .as_ref()
            .map(|surface| &surface.mode)
        else {
            return;
        };
        let root = du.root.clone();
        let view = du.view.clone();
        let dock_owner = self.disk_usage_dock_owner(cx);

        match crate::disk_usage::open_existing_window(
            root.clone(),
            view.clone(),
            Some(dock_owner),
            cx,
        ) {
            Ok(_) => {
                ToolResultSurface::disk_usage(root, view)
                    .handle_host_event(ToolHostEvent::HostChanged(ToolHostContext::Windowed), cx);
                self.close_active_tool_result(cx);
            }
            Err(e) => {
                crate::log_warn!(90, "disk-usage: pop-out failed: {e:?}");
                window.push_notification(
                    error_notification(
                        tr!(
                            "Could not pop out Disk Usage: {detail}",
                            detail = format!("{e:?}")
                        )
                        .to_string(),
                    ),
                    cx,
                );
            }
        }
    }

    fn on_pop_out_disk_usage(
        &mut self,
        _: &PopOutDiskUsage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pop_out_active_disk_usage(window, cx);
    }

    /// Find Duplicates — scan the active tab's directory for duplicate
    /// files and show them grouped in the tab.
    pub fn on_find_duplicates(
        &mut self,
        _: &FindDuplicates,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Find Duplicates");
        let tab_id = self.active_tab().id;
        self.start_duplicate_scan(
            tab_id,
            ferail_fs_native::DupeMode::Exact,
            Some(window.window_handle()),
            cx,
        );
    }

    /// Find Similar Images — the duplicate finder panel with a perceptual,
    /// entirely in-memory image funnel.
    pub fn on_find_similar_images(
        &mut self,
        _: &FindSimilarImages,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Find Similar Images");
        let tab_id = self.active_tab().id;
        self.start_duplicate_scan(
            tab_id,
            ferail_fs_native::DupeMode::Similar,
            Some(window.window_handle()),
            cx,
        );
    }

    /// Toolbar Sort menu. Each `SortBy*` action picks a column;
    /// re-selecting the active column flips its direction. First pick
    /// of a column uses a Finder-like default (Name/Kind ascending,
    /// Size largest-first, Modified newest-first). Pure in-memory
    /// re-sort of the already-enumerated rows.
    fn set_sort_column(&mut self, col: crate::file_list::SortColumn, cx: &mut Context<Self>) {
        use crate::file_list::SortColumn;
        let table = self.active_tab().table.clone();
        let current = self
            .process
            .list_sort
            .get()
            .unwrap_or((SortColumn::Name, true));
        let ascending = match current {
            (c, a) if c == col => !a,
            // Ant Trail joins Size/Modified in defaulting to descending:
            // the interesting end of a heat ranking is the hot end.
            _ => matches!(col, SortColumn::Name | SortColumn::Format),
        };
        crate::file_list::apply_sort_column(&table, col, ascending, cx);
        cx.notify();
    }

    pub fn on_sort_by_name(&mut self, _: &SortByName, _: &mut Window, cx: &mut Context<Self>) {
        self.set_sort_column(crate::file_list::SortColumn::Name, cx);
    }

    pub fn on_sort_by_size(&mut self, _: &SortBySize, _: &mut Window, cx: &mut Context<Self>) {
        self.set_sort_column(crate::file_list::SortColumn::Size, cx);
    }

    pub fn on_sort_by_kind(&mut self, _: &SortByKind, _: &mut Window, cx: &mut Context<Self>) {
        self.set_sort_column(crate::file_list::SortColumn::Format, cx);
    }

    pub fn on_sort_by_modified(
        &mut self,
        _: &SortByModified,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_sort_column(crate::file_list::SortColumn::Modified, cx);
    }

    /// Sort by Ant Trail heat — the folders this user opens most, on
    /// top (docs/features/ANT_TRAIL.md). Reads the heat the delegate
    /// already cached per row, so it is the same in-memory re-sort as
    /// every other sort pick.
    pub fn on_sort_by_ant_trail(
        &mut self,
        _: &SortByAntTrail,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_sort_column(crate::file_list::SortColumn::AntTrail, cx);
    }

    /// Cmd+Y — open the viewer window (docs/features/VIEWER.md) on a
    /// snapshot of the current tab's visible files (sorted + filtered
    /// order, directories skipped), starting at the lead row. The
    /// snapshot is in-memory only: entries are already enumerated and
    /// paths resolve through the NodeStore, so no filesystem I/O
    /// happens on this path.
    pub fn on_open_viewer(&mut self, _: &OpenViewer, window: &mut Window, cx: &mut Context<Self>) {
        let mut playlist = Vec::new();
        let mut start = 0usize;
        {
            let tab = self.active_tab();
            let entries = &tab.table.read(cx).delegate().entries;
            let lead = tab.lead_row(entries);
            for (ix, e) in entries.iter().enumerate() {
                if matches!(e.kind, EntryKind::Directory) {
                    continue;
                }
                if lead == Some(ix) {
                    start = playlist.len();
                }
                if let Some(path) = self.path_for_row(ix, cx) {
                    playlist.push(crate::viewer::PlaylistEntry {
                        path,
                        name: e.name.to_string(),
                    });
                }
            }
        }
        if playlist.is_empty() {
            use gpui_component::notification::Notification;
            window.push_notification(
                Notification::info(tr!("No files to view in this folder")),
                cx,
            );
            return;
        }
        crate::viewer::open_viewer(playlist, start, window, cx);
    }

    /// File-list context action — open the viewer anchored to the
    /// right-clicked file and start the slideshow immediately
    /// (docs/features/VIEWER.md). Same folder playlist as `OpenViewer`,
    /// but `start` follows the context row rather than the lead.
    pub fn on_slideshow_from_here(
        &mut self,
        _: &SlideshowFromHere,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let anchor = self.target_row(cx);
        let mut playlist = Vec::new();
        let mut start = 0usize;
        {
            let tab = self.active_tab();
            let entries = &tab.table.read(cx).delegate().entries;
            for (ix, e) in entries.iter().enumerate() {
                if matches!(e.kind, EntryKind::Directory) {
                    continue;
                }
                if anchor == Some(ix) {
                    start = playlist.len();
                }
                if let Some(path) = self.path_for_row(ix, cx) {
                    playlist.push(crate::viewer::PlaylistEntry {
                        path,
                        name: e.name.to_string(),
                    });
                }
            }
        }
        if playlist.is_empty() {
            use gpui_component::notification::Notification;
            window.push_notification(
                Notification::info(tr!("No files to view in this folder")),
                cx,
            );
            return;
        }
        crate::viewer::open_viewer_playing(playlist, start, window, cx);
    }

    /// Cmd+P — toggle preview-pane visibility. The pane defaults to
    /// shown; toggling off gives the file list the full content width.
    fn on_toggle_preview(&mut self, _: &TogglePreview, _: &mut Window, cx: &mut Context<Self>) {
        self.preview_visible = !self.preview_visible;
        // When revealing the pane, make sure the current lead selection's
        // preview is requested. Selection-change normally kicks this off, but
        // request again so an explicit Cmd+P never reveals an empty pane (e.g.
        // if the cached entry was evicted, or selection predates the toggle).
        if self.preview_visible {
            let lead = {
                let entries = &self.active_tab().table.read(cx).delegate().entries;
                self.active_tab().lead_row(entries)
            };
            if let Some(row) = lead {
                self.request_preview_for_row(row, cx);
            }
        }
        cx.notify();
    }

    /// Cmd+I — open the Get Info popup for the target row (the right-click
    /// row, else the lead selection). With nothing selected, gets info on
    /// the tab's current folder, matching Finder.
    pub(crate) fn on_get_info(&mut self, _: &GetInfo, window: &mut Window, cx: &mut Context<Self>) {
        use ferail_core::entry_info::InfoTarget;
        let targets = self.action_entries_visible_order(cx);
        // Nothing targeted → info on the current folder (Finder parity).
        if targets.is_empty() {
            let dir = self.active_tab().current_dir.clone();
            let name = dir
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| dir.display().to_string());
            let weak = cx.weak_entity();
            crate::entry_info::open(dir, name, InfoTarget::Folder, None, weak, cx);
            return;
        }
        // FanOut: one Get Info window per file (each `entry_info::open`
        // spawns its own window). Guard the noisy case behind a confirm.
        let count = targets.len();
        self.confirm_fanout(
            count,
            tr!("Get Info?"),
            trn!(
                "Open {n} Get Info window?",
                "Open {n} Get Info windows?",
                count
            )
            .to_string(),
            tr!("Get Info"),
            window,
            cx,
            move |_this, _window, cx| {
                for (_, entry, path) in targets {
                    let target = match entry.kind {
                        EntryKind::Directory => InfoTarget::Folder,
                        _ => InfoTarget::File,
                    };
                    // Reuse the file list's recursive folder size if known.
                    let known_size = if matches!(target, InfoTarget::Folder) && entry.size > 0 {
                        Some(entry.size)
                    } else {
                        None
                    };
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(str::to_string)
                        .unwrap_or_default();
                    let weak = cx.weak_entity();
                    crate::entry_info::open(path, name, target, known_size, weak, cx);
                }
            },
        );
    }

    /// Push the current UI zoom into the gpui-component theme's base
    /// font size. `Root::render` copies `theme.font_size` into the
    /// window rem size every frame, and all chrome text is rem-relative
    /// through the `crate::text` design tokens, so this is the
    /// framework-native zoom hook: it scales the whole window —
    /// including Root-level overlays (notifications, dialogs) — rather
    /// than fighting `Root` by overriding `rem_size` on just the shell
    /// subtree. (Icon and fixed-px layout scaling are still TODO.)
    pub(crate) fn apply_ui_zoom(&self, cx: &mut App) {
        if cx.has_global::<gpui_component::Theme>() {
            gpui_component::Theme::global_mut(cx).font_size =
                px(crate::text::BASE_REM_PX * self.ui_scale);
        }
    }

    /// Cmd+= / Cmd+- / Cmd+0 — UI zoom. Bumps `ui_scale` by ±0.1
    /// (clamped to [0.6, 2.0]) and re-applies it to the theme base font
    /// size; `window.refresh()` forces `Root` to re-read it this frame.
    fn on_zoom_in(&mut self, _: &ZoomIn, window: &mut Window, cx: &mut Context<Self>) {
        self.ui_scale = (self.ui_scale + 0.1).clamp(0.6, 2.0);
        self.apply_ui_zoom(cx);
        window.refresh();
    }
    fn on_zoom_out(&mut self, _: &ZoomOut, window: &mut Window, cx: &mut Context<Self>) {
        self.ui_scale = (self.ui_scale - 0.1).clamp(0.6, 2.0);
        self.apply_ui_zoom(cx);
        window.refresh();
    }
    fn on_zoom_reset(&mut self, _: &ZoomReset, window: &mut Window, cx: &mut Context<Self>) {
        self.ui_scale = 1.0;
        self.apply_ui_zoom(cx);
        window.refresh();
    }

    /// Open the selected folder(s) in a new tab (context-menu command).
    /// Only folders can seed a tab, so file targets are dropped; falls back
    /// to the active tab's current dir when nothing is targeted.
    fn on_open_in_new_tab(
        &mut self,
        _: &OpenInNewTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets = self.action_entries_visible_order(cx);
        // Nothing targeted (e.g. background invocation) → open the current
        // folder in a new tab, matching the prior single-target fallback.
        if targets.is_empty() {
            let path = self.active_tab().current_dir.clone();
            self.open_path_in_new_tab(path, window, cx);
            return;
        }
        // FanOut: a tab per folder (Finder's "Open in New Tabs"); a tab is a
        // folder view, so files can't anchor one and are skipped — matching
        // the menu, which hides the item on a file anchor
        // (`file_list::avail_anchor_dir`). A file-only target set is a no-op.
        let folders: Vec<PathBuf> = targets
            .into_iter()
            .filter(|(_, e, _)| matches!(e.kind, EntryKind::Directory))
            .map(|(_, _, p)| p)
            .collect();
        let count = folders.len();
        if count == 0 {
            return;
        }
        if count == 1 {
            self.open_path_in_new_tab(folders[0].clone(), window, cx);
            return;
        }
        self.confirm_fanout(
            count,
            tr!("Open in New Tabs?"),
            trn!(
                "Open {n} folder in new tabs?",
                "Open {n} folders in new tabs?",
                count
            )
            .to_string(),
            tr!("Open"),
            window,
            cx,
            move |this, window, cx| {
                for path in folders {
                    this.open_path_in_new_tab(path, window, cx);
                }
            },
        );
    }

    /// Push a new tab at `path` and switch to it. Shared entry point
    /// for modifier-click in the file list / sidebar / Favorites
    /// section so each surface doesn't reimplement the tab push.
    /// Inserts beside the active tab per spec §3.3.
    pub fn open_path_in_new_tab(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.process.fs.id_for_path(&path);
        self.process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(path.clone(), id);
        let tab = self.make_tab(path, id, window, cx);
        let insert_at = self.active + 1;
        self.tabs.insert(insert_at, tab);
        self.active = insert_at;
        let cur = self.active_tab().current_dir.clone();
        self.load_path(cur, cx);
    }

    fn on_open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        // Spawn a second native window hosting the SettingsView,
        // matching macOS convention where Preferences is its own
        // window not a modal sheet. Independent of the file-manager
        // shell's lifecycle — closing one doesn't close the other.
        let _ = window;
        crate::settings::open_settings_window(cx);
    }

    pub fn navigate_back(&mut self, cx: &mut Context<Self>) {
        if self.active_tab().platform_namespace.is_some() {
            let tab_id = self.active_tab().id;
            let request = self
                .active_tab_mut()
                .platform_namespace
                .as_mut()
                .and_then(|session| session.go_back());
            if let Some((request, cancel)) = request {
                self.start_platform_listing(tab_id, request, cancel, cx);
            }
            return;
        }
        let (path, mut snapshot, came_from_path) = {
            let tab = self.active_tab_mut();
            if tab.history_index == 0 {
                return;
            }
            let came_from_path = tab.current_dir.clone();
            // Save the current entry's selection before stepping
            // back, so a subsequent Forward restores it.
            if let Some(cur) = tab.history.get_mut(tab.history_index) {
                cur.selection = if tab.selection_all {
                    HashSet::new()
                } else {
                    tab.selection.clone()
                };
                cur.anchor = tab.anchor;
                cur.lead = tab.lead;
            }
            tab.history_index -= 1;
            let entry = tab.history[tab.history_index].clone();
            (entry.path.clone(), entry, came_from_path)
        };
        if let Some(child_path) = history_child_to_select(&came_from_path, &path) {
            let child_id = self.process.fs.id_for_path(&child_path);
            snapshot.selection.clear();
            snapshot.selection.insert(child_id);
            snapshot.anchor = Some(child_id);
            snapshot.lead = Some(child_id);
        }
        crate::trail::navigate(crate::trail::NavKind::Back, &path);
        self.restore_from_history(snapshot, path, cx);
    }

    pub fn navigate_forward(&mut self, cx: &mut Context<Self>) {
        if self.active_tab().platform_namespace.is_some() {
            let tab_id = self.active_tab().id;
            let request = self
                .active_tab_mut()
                .platform_namespace
                .as_mut()
                .and_then(|session| session.go_forward());
            if let Some((request, cancel)) = request {
                self.start_platform_listing(tab_id, request, cancel, cx);
            }
            return;
        }
        let (path, snapshot) = {
            let tab = self.active_tab_mut();
            if tab.history_index + 1 >= tab.history.len() {
                return;
            }
            if let Some(cur) = tab.history.get_mut(tab.history_index) {
                cur.selection = if tab.selection_all {
                    HashSet::new()
                } else {
                    tab.selection.clone()
                };
                cur.anchor = tab.anchor;
                cur.lead = tab.lead;
            }
            tab.history_index += 1;
            let entry = tab.history[tab.history_index].clone();
            (entry.path.clone(), entry)
        };
        crate::trail::navigate(crate::trail::NavKind::Forward, &path);
        self.restore_from_history(snapshot, path, cx);
    }

    /// Common back/forward landing: seed the tab's selection from
    /// the history entry's snapshot, then issue a reload of the
    /// destination. The reload preserves selection through
    /// `load_path` (no longer clears it), and `finish_directory_load`
    /// reconciles the snapshot against the freshly streamed model —
    /// dropping NodeIds that no longer exist.
    fn restore_from_history(
        &mut self,
        snapshot: HistoryEntry,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        {
            let tab = self.active_tab_mut();
            tab.selection_all = false;
            tab.selection = snapshot.selection;
            tab.anchor = snapshot.anchor;
            tab.lead = snapshot.lead;
            tab.filtered_out.clear();
            tab.range_live = false;
        }
        self.active_tab_mut().pending_select_row = None;
        self.active_tab_mut().pending_select_rows.clear();
        let node_id = self.process.fs.id_for_path(&path);
        self.active_tab_mut().nav.navigate_to(node_id);
        self.record_ant_visit(node_id, cx);
        self.load_path(path, cx);
    }

    /// Persist last-dir + show-hidden + UI-scale to disk off the UI
    /// thread. Even though the state file is tiny, navigation must
    /// not wait on app-support directory creation or disk writes.
    /// `theme_pref` is owned by the Settings entity — persisted there
    /// after a tile click, not from Shell.
    fn save_state_async(&self, cx: &mut Context<Self>) {
        let last_dir = self.active_tab().current_dir.clone();
        let show_hidden = self.show_hidden;
        let ui_scale = self.ui_scale;
        cx.background_executor()
            .spawn(async move {
                let mut s = app_state::load();
                s.last_dir = Some(last_dir);
                s.show_hidden = Some(show_hidden);
                s.ui_scale = Some(ui_scale);
                app_state::save(&s);
            })
            .detach();
    }

    /// Inner load: re-enumerate the directory + refresh the table +
    /// re-target the watcher. Does **not** touch history (history
    /// is only mutated by `navigate`). Loads into the currently
    /// active tab. Public so the screenshot CLI driver can call it
    /// directly after seeding tab state.
    pub fn load_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let tab_id = self.active_tab().id;
        self.load_path_for_tab(tab_id, path, cx);
    }

    /// Schedule a directory load against a specific tab. Used by
    /// `load_path` (active tab) and, in Phase A+B + beyond, any
    /// background path that wants to retarget an inactive tab —
    /// e.g. the cross-window reload fan-out in Phase E.
    pub fn load_path_for_tab(&mut self, tab_id: TabId, path: PathBuf, cx: &mut Context<Self>) {
        let Some(tab_index) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        // A real filesystem target always leaves the specialized namespace
        // surface. Dropping the tab-owned session cancels its provider worker
        // and releases all opaque item identities in one operation.
        self.tabs[tab_index].platform_namespace = None;
        let node_id = self.process.fs.id_for_path(&path);
        self.process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(path.clone(), node_id);
        // In-place reload: re-reading the directory already on screen
        // (Refresh, Esc clear-filter, show-hidden, watcher reload).
        // Stage the new listing off-screen and swap it in atomically on
        // `Done` so the live rows never collapse to the first batch and
        // stream back — that collapse/refill is the visible flicker.
        // Fresh navigation (`path` differs, or nothing on screen yet)
        // keeps the progressive streaming reveal.
        let reload_in_place = self.tabs[tab_index].current_dir == path
            && !self.tabs[tab_index]
                .table
                .read(cx)
                .delegate()
                .entries
                .is_empty();
        let tab = &mut self.tabs[tab_index];
        tab.nav.replace_current(node_id);
        tab.current_dir = path.clone();
        // Selection is preserved across `load_path` calls so
        // filter/refresh/show-hidden/watcher reloads can reconcile
        // against the new model (spec §2.6). `navigate`, which
        // commits a new path, clears selection itself BEFORE
        // delegating here.
        tab.last_error = None;
        // Default every load to cache-first prefetch and cache-first
        // folder sizes. `on_refresh` re-arms both for its own load after
        // delegating here.
        tab.force_resniff = false;
        tab.force_folder_sizes = false;
        // Zero the skipped-entry aggregates so a cancelled/failed load
        // can't leave a stale chip on screen; the new load's Done
        // rewrites both.
        tab.hidden_summary = Default::default();
        tab.filter_summary = Default::default();
        tab.load_generation = tab.load_generation.wrapping_add(1);
        let generation = tab.load_generation;
        let filter = tab.filter_text.clone();
        let show_hidden = self.show_hidden;
        crate::obs::breadcrumb(format_args!(
            "navigation load generation={generation} reload={reload_in_place} filter={} hidden={show_hidden}",
            !filter.is_empty()
        ));

        if let Some(cancel) = self.tabs[tab_index].load_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = self.tabs[tab_index].wsl_resolve_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        // The folder-size and prefetch passes for the previous listing
        // are also obsolete — stop their walks before the new
        // enumeration starts competing for disk I/O.
        if let Some(cancel) = self.tabs[tab_index].folder_size_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = self.tabs[tab_index].prefetch_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        // File-detail enrichment is table/viewport-owned now. Stop the old
        // surface immediately rather than letting its bounded worker keep
        // opening files until the replacement listing reaches Done.
        self.tabs[tab_index].table.update(cx, |state, _cx| {
            state.delegate_mut().cancel_visible_details();
        });
        let task = self.process.tasks.borrow_mut().begin(
            TaskKind::Enumeration,
            tr!(
                "Reading {path}",
                path = middle_truncate_path(&path.to_string_lossy(), 40)
            )
            .to_string(),
            true,
        );
        if let Some(previous) = self.tabs[tab_index].load_task.replace(task) {
            self.process.tasks.borrow_mut().end(previous);
        }
        self.tabs[tab_index].load_pending_first_batch = true;
        self.tabs[tab_index].load_staging = reload_in_place.then(|| LoadBatch {
            entries: Vec::new(),
            paths: HashMap::new(),
        });

        // Point the watcher at the new directory. This only queues a
        // command — the OS registration (which can block on a
        // spun-down drive; notify's FSEvents `watch()` canonicalizes
        // the path) happens on the watcher's worker thread. Failures
        // there (path doesn't exist, watcher saturated) are non-fatal:
        // the user still gets the listing; they just lose live updates.
        if let Some(w) = self.process.watcher.borrow_mut().as_mut() {
            w.watch(&path);
        }
        // Drop watches no live tab shows anymore (this navigation may
        // have been the last reference to the previous directory).
        let own_dirs: Vec<PathBuf> = self.tabs.iter().map(|t| t.current_dir.clone()).collect();
        self.process.prune_watches(cx.entity_id(), own_dirs, cx);
        self.save_state_async(cx);

        let fs = self.process.fs.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.tabs[tab_index].load_cancel = Some(cancel.clone());
        let (tx, rx) = async_channel::unbounded();
        let worker_path = path.clone();
        cx.background_executor()
            .spawn(async move {
                run_directory_load_streaming(fs, worker_path, show_hidden, filter, cancel, tx);
            })
            .detach();

        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                let done = matches!(msg, LoadMsg::Done(..));
                let stale = this
                    .update(cx, |this, cx| {
                        // Find the loading tab by id — its index may
                        // have shifted under reorder, and it may have
                        // closed entirely.
                        let Some(idx) = this.tabs.iter().position(|t| t.id == tab_id) else {
                            return true;
                        };
                        if this.tabs[idx].load_generation != generation
                            || this.tabs[idx].current_dir != path
                        {
                            return true;
                        }
                        // Helpers address the loading tab by index —
                        // the Phase A+B "swap self.active and restore"
                        // hack is gone. It was re-entrancy-fragile: an
                        // observer firing synchronously inside the
                        // apply (e.g. the favorites subscription) read
                        // `active_tab()` and saw the loading tab
                        // instead of the user's.
                        this.apply_directory_load_msg_in_tab(idx, msg, cx);
                        false
                    })
                    .unwrap_or(true);
                if stale || done {
                    break;
                }
            }
        })
        .detach();

        // Slow-device feedback (docs/ARCHITECTURE.md#prime-directive): if
        // the first batch is still missing after the delay — an
        // external drive spinning up, a cold network mount — swap the
        // pane to the skeleton loading view. The status bar's
        // indeterminate stripe is already running (the Enumeration
        // task above); this adds the in-pane signal and retires the
        // previous directory's stale, still-clickable rows.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(SLOW_LOAD_INDICATOR_DELAY)
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(idx) = this.tabs.iter().position(|t| t.id == tab_id) else {
                    return;
                };
                let tab = &this.tabs[idx];
                // Only a fresh navigation still waiting for its first
                // batch qualifies: a superseded generation belongs to
                // a navigation the user already left, and an in-place
                // reload (`load_staging`) keeps its live rows visible
                // by design.
                if tab.load_generation != generation
                    || !tab.load_pending_first_batch
                    || tab.load_staging.is_some()
                {
                    return;
                }
                let label = tab
                    .current_dir
                    .file_name()
                    .map(|n| {
                        ferail_fs_native::paths::display_leaf(&n.to_string_lossy()).into_owned()
                    })
                    .unwrap_or_else(|| tab.current_dir.to_string_lossy().into_owned());
                this.tabs[idx].table.update(cx, |state, cx| {
                    state.delegate_mut().slow_load = Some(label.into());
                    state.refresh(cx);
                });
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_directory_load_msg_in_tab(
        &mut self,
        idx: usize,
        msg: LoadMsg,
        cx: &mut Context<Self>,
    ) {
        match msg {
            LoadMsg::Batch(batch) => self.apply_directory_batch_in_tab(idx, batch, cx),
            LoadMsg::Done(error, hidden, filtered) => {
                self.finish_directory_load_in_tab(idx, error, hidden, filtered, cx)
            }
        }
    }

    fn apply_directory_batch_in_tab(
        &mut self,
        idx: usize,
        batch: LoadBatch,
        cx: &mut Context<Self>,
    ) {
        for (id, path) in &batch.paths {
            self.process
                .node_store
                .borrow_mut()
                .get_or_create_path_with_id(path.clone(), *id);
        }
        // In-place reload: accumulate off-screen, leaving the live rows
        // untouched. The complete listing swaps in at `Done`
        // (`finish_directory_load_in_tab`) — no clear, no collapse, no
        // flicker. Selection / favorited / range passes also wait for
        // the swap so they reconcile against the final model once.
        if let Some(staging) = self.tabs.get_mut(idx).and_then(|t| t.load_staging.as_mut()) {
            staging.entries.extend(batch.entries);
            staging.paths.extend(batch.paths);
            return;
        }
        let heats: Vec<f32> = batch
            .entries
            .iter()
            .map(|entry| self.ant_heat(entry.id))
            .collect();
        let favorites: Vec<bool> = {
            let favs = self.process.favorites();
            let favs = favs.read(cx);
            batch
                .entries
                .iter()
                .map(|entry| {
                    batch
                        .paths
                        .get(&entry.id)
                        .is_some_and(|path| favs.contains_path(path))
                })
                .collect()
        };
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        let first_batch = tab.load_pending_first_batch;
        tab.load_pending_first_batch = false;
        let table = tab.table.clone();
        table.update(cx, |state, cx| {
            if first_batch {
                state.delegate_mut().clear();
            }
            state.delegate_mut().append_entries_decorated(
                batch.entries,
                batch.paths,
                heats,
                favorites,
            );
            state.refresh(cx);
        });
        // Spec §2.6 streaming arrival passes:
        //   1. Mirror current selection state into the delegate so
        //      the parallel render view paints the rows that just
        //      arrived.
        //   2. Lift any filtered-out NodeIds back into the live
        //      selection if their rows have now streamed in.
        //   3. Recompute a still-live Shift-range so rows landing
        //      between anchor and lead join the selection.
        self.refresh_file_list_selection_in_tab(idx, cx);
        self.restore_filtered_out_against_model_in_tab(idx, cx);
        self.recompute_live_range_in_tab(idx, cx);
        // Consume any queued screenshot-driver row select now that
        // the model has data.
        self.apply_pending_select_row_in_tab(idx, cx);
        cx.notify();
    }

    /// Recompute the file list's per-row `is_favorited` parallel vec
    /// from the current Favorites entity index. Called from:
    /// - `apply_directory_batch` (after rows arrive)
    /// - The `cx.observe(&self.process.favorites(), …)` subscription registered
    ///   in `Shell::new` (so add / remove / repoint repaints star
    ///   badges in the same frame, §5.3).
    pub fn refresh_file_list_favorited(&mut self, cx: &mut Context<Self>) {
        let idx = self.active;
        self.refresh_file_list_favorited_in_tab(idx, cx);
    }

    /// Tab-explicit variant used by the streaming pipeline.
    fn refresh_file_list_favorited_in_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        let favs = self.process.favorites().clone();
        let table = tab.table.clone();
        let favs_ref = favs.read(cx);
        // Pre-collect each row's path so the table-update closure
        // doesn't need to borrow Shell again.
        let bits: Vec<bool> = {
            let state = table.read(cx);
            let delegate = state.delegate();
            delegate
                .entries
                .iter()
                .map(|entry| {
                    delegate
                        .path_for_entry(entry.id)
                        .is_some_and(|path| favs_ref.contains_path(&path))
                })
                .collect()
        };
        let _ = favs_ref;
        table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            // Resize defensively — the table may have been cleared
            // between the snapshot and this update.
            delegate.is_favorited.resize(delegate.entries.len(), false);
            for (i, b) in bits.into_iter().enumerate() {
                if let Some(slot) = delegate.is_favorited.get_mut(i) {
                    *slot = b;
                }
            }
            state.refresh(cx);
        });
    }

    /// Populate the file list's per-row Finder colour tags (the §5 dot
    /// chips the list row and grid cell paint from `delegate.tags`).
    ///
    /// Tags live in xattrs, so reading them is filesystem I/O — barred
    /// from the UI thread by the Prime Directive. Unlike the in-memory
    /// favorited refresh, this snapshots each row's `(NodeId, path)`,
    /// reads `read_canonical_tags` on the background executor, then
    /// applies the result back **by NodeId**: if the listing was
    /// replaced by a fresh navigation while the read was in flight, the
    /// stale ids simply don't match and are dropped. Fired once per
    /// completed load from `finish_directory_load_in_tab`. (The old
    /// synchronous `FileListDelegate::load()` read these inline; that
    /// path is dead now that loads stream through `append_entries`,
    /// which stubs the tag slots empty.)
    fn refresh_file_list_tags_in_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        // Finder-tag reads are xattr I/O, part of the file-detail scan the
        // Performance toggle gates. Off, rows render tagless (no per-row
        // xattr read). Gated inside the fn so every caller — the load-done
        // pass and the post-file-op refresh — respects the setting.
        if !crate::prefetch::file_detail_scan_enabled(cx) {
            return;
        }
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        let tab_id = tab.id;
        let table = tab.table.clone();
        // Cap the read so a pathological directory (a 100k-entry
        // Downloads) can't queue an unbounded pile of xattr reads off
        // one navigation. Beyond the cap rows render tagless, matching
        // the original `load()` policy (its cap was 200; off-thread we
        // can afford a more generous bound).
        const TAG_READ_CAP: usize = 1000;
        let targets: Vec<(NodeId, PathBuf)> = {
            let state = table.read(cx);
            let del = state.delegate();
            del.entries
                .iter()
                .take(TAG_READ_CAP)
                .filter_map(|e| del.path_for_entry(e.id).map(|p| (e.id, p)))
                .collect()
        };
        if targets.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let tagged: Vec<(NodeId, Vec<ferail_core::commands::TagColor>)> = cx
                .background_executor()
                .spawn(async move {
                    targets
                        .into_iter()
                        .map(|(id, p)| (id, crate::platform_shell::read_canonical_tags(&p)))
                        .collect()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(tab) = this.tabs.iter().find(|t| t.id == tab_id) else {
                    return;
                };
                let table = tab.table.clone();
                table.update(cx, |state, cx| {
                    let mut by_id: std::collections::HashMap<
                        NodeId,
                        Vec<ferail_core::commands::TagColor>,
                    > = tagged.into_iter().collect();
                    let del = state.delegate_mut();
                    // Defensive resize — the model may have grown or
                    // shrunk between the snapshot and this apply.
                    let ids: Vec<NodeId> = del.entries.iter().map(|e| e.id).collect();
                    del.tags.resize(ids.len(), Vec::new());
                    let mut changed = false;
                    for (i, id) in ids.iter().enumerate() {
                        if let Some(t) = by_id.remove(id) {
                            del.tags[i] = t;
                            changed = true;
                        }
                    }
                    if changed {
                        state.refresh(cx);
                    }
                });
            });
        })
        .detach();
    }

    fn finish_directory_load_in_tab(
        &mut self,
        idx: usize,
        error: Option<EnumerationError>,
        hidden: crate::shell::loading::HiddenSummary,
        filtered: crate::shell::loading::FilterSummary,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        // The load is complete, so the skipped-hidden and filtered-out
        // totals are final. (Zeroed at load start, so a mid-stream
        // cancel never shows a half count.)
        tab.hidden_summary = hidden;
        tab.filter_summary = filtered;
        if let Some(id) = tab.load_task.take() {
            self.process.tasks.borrow_mut().end(id);
        }
        self.tabs[idx].load_cancel = None;
        if let Some(staged) = self.tabs[idx].load_staging.take() {
            // In-place reload finished: swap the complete listing in
            // atomically over the still-visible old rows. One rebuild,
            // no intermediate empty/partial state — the refresh is
            // flicker-free. Reconcile passes run once against the final
            // model, mirroring the streaming path's per-batch passes.
            self.tabs[idx].load_pending_first_batch = false;
            // `replace_entries` reapplies the delegate's effective sort while
            // keeping row-parallel vectors aligned. Re-enumeration arrives in
            // raw readdir order, so the swap must not inherit that order.
            let heats: Vec<f32> = staged
                .entries
                .iter()
                .map(|entry| self.ant_heat(entry.id))
                .collect();
            let table = self.tabs[idx].table.clone();
            table.update(cx, |state, cx| {
                state
                    .delegate_mut()
                    .replace_entries(staged.entries, staged.paths, heats);
                state.refresh(cx);
            });
            self.refresh_file_list_favorited_in_tab(idx, cx);
            self.refresh_file_list_selection_in_tab(idx, cx);
            self.restore_filtered_out_against_model_in_tab(idx, cx);
            self.recompute_live_range_in_tab(idx, cx);
            self.apply_pending_select_row_in_tab(idx, cx);
        } else if self.tabs[idx].load_pending_first_batch {
            self.tabs[idx].load_pending_first_batch = false;
            let table = self.tabs[idx].table.clone();
            table.update(cx, |state, cx| {
                state.delegate_mut().clear();
                state.refresh(cx);
            });
        } else {
            // Streaming appends stay in raw enumeration order so every batch
            // is O(batch). Apply the effective sort once, after the final
            // batch, instead of re-sorting the accumulated model hundreds or
            // thousands of times.
            let table = self.tabs[idx].table.clone();
            table.update(cx, |state, cx| {
                state.delegate_mut().apply_effective_sort();
                state.refresh(cx);
            });
        }
        // The listing is final: queued post-op names that didn't resolve
        // never will (hidden file with Show Hidden off, filtered out, or
        // gone again) — drop them so they can't select a same-named entry
        // after a later reload or navigation. Active tab only: a
        // background tab's queue hasn't had its apply chance yet (the
        // apply passes are active-tab-scoped).
        if idx == self.active {
            self.tabs[idx].pending_select_names.clear();
        }
        // Tell the table how many rows the filter took away, so its
        // empty state says "filtered out" instead of "this folder is
        // empty" when the needle matched nothing. Written after the
        // replace/clear above, which reset it.
        let filtered_out = filtered.count;
        self.tabs[idx].table.update(cx, |state, _cx| {
            state.delegate_mut().filtered_out = filtered_out;
        });
        let row_count = self.tabs[idx].table.read(cx).delegate().entries.len();
        if row_count == 0 {
            let try_wsl_symlink =
                error.is_some() && crate::platform_shell::is_wsl_path(&self.tabs[idx].current_dir);
            self.tabs[idx].last_error = error;
            if try_wsl_symlink {
                self.start_wsl_symlink_recovery(idx, cx);
            }
        } else {
            if let Some(err) = error {
                crate::log_warn!(90, "directory load ended with partial rows: {err:?}");
            }
            self.tabs[idx].last_error = None;
            // Finder colour tags for the now-complete listing. xattr
            // reads are filesystem I/O, so this runs off-thread and
            // reports back — unlike the cheap per-batch favorited
            // refresh, it fires once here at `Done`.
            self.refresh_file_list_tags_in_tab(idx, cx);
        }

        // Spec §2.6 `Done`: drop NodeIds no longer in the final
        // model (or hold them in `filtered_out` when a filter is
        // active), re-seat anchor / lead if they vanished. Runs
        // once per load; iter-1 navigation that cleared selection
        // upfront makes this a no-op there, but back/forward and
        // any future external-mutation reload route through here.
        self.reconcile_done_in_tab(idx, cx);

        // Stage 4: arm viewport-owned magic/description/quarantine warming
        // after the foreground table has received its final snapshot. The
        // table callback schedules only visible rows as the user scrolls;
        // opening a 10k- or million-file directory never starts 10k/million
        // file opens. Refresh still bypasses cached magic/description once for
        // every row that reaches the viewport (see `Tab::force_resniff`).
        let force_resniff = self.tabs[idx].force_resniff;
        let table = self.tabs[idx].table.clone();
        table.update(cx, |state, _cx| {
            state.delegate_mut().enable_visible_details(force_resniff);
        });
        let fs = self.process.fs.clone();
        let db = self.process.db_snapshot();
        let tasks = self.process.tasks.clone();
        // Folder sizes for the directory rows: cache-validated
        // against each folder's mtime, recomputed off-thread on
        // miss, streamed back as they resolve. Skipped entirely when
        // the user has disabled folder sizing (Settings → Performance)
        // — the Size column then shows a dash for directories.
        // WSL roots are path-backed but remote/service-mediated. The current
        // folder-size pass recursively walks *every* directory row, so running
        // it here would defeat viewport-bounded WSL browsing. Keep the Size
        // column's folder totals unset until that worker gains an explicit
        // on-demand/viewport mode; file sizes and all viewport details remain.
        if crate::folder_sizes::folder_sizing_enabled(cx)
            && !crate::platform_shell::is_wsl_path(&self.tabs[idx].current_dir)
        {
            let size_cancel = Arc::new(AtomicBool::new(false));
            let size_tab_id = self.tabs[idx].id;
            let size_generation = self.tabs[idx].load_generation;
            // Only an explicit Refresh bypasses the cache (`Tab::force_folder_sizes`).
            // Plain navigation — including coming back to a folder you just
            // left — answers from the `folder_sizes` rows the last pass wrote.
            let force_sizes = self.tabs[idx].force_folder_sizes;
            self.tabs[idx].folder_size_cancel = Some(size_cancel.clone());
            crate::folder_sizes::start(
                table,
                fs,
                db,
                tasks,
                size_tab_id,
                size_generation,
                size_cancel,
                force_sizes,
                cx,
            );
        }
        let icon_seeds = self.icon_seeds_from_table_in_tab(idx, cx);
        self.start_icon_warm(icon_seeds, cx);
        // Real thumbnails and file details for the first screen — without
        // this they'd only appear after the first scroll on a folder whose
        // visible range matches the previous one's.
        self.warm_loaded_viewport_in_tab(idx, cx);
        self.refresh_volume_info_in_tab(idx, cx);
        cx.notify();
    }

    /// Windows UNC enumeration cannot follow some Linux symlinks exposed by
    /// WSL. Direct `NativeFs` enumeration is always attempted first; only an
    /// empty failed load reaches this bounded provider fallback.
    fn start_wsl_symlink_recovery(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        let tab_id = tab.id;
        let generation = tab.load_generation;
        let original = tab.current_dir.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        tab.wsl_resolve_cancel = Some(cancel.clone());
        cx.spawn(async move |this, cx| {
            let worker_path = original.clone();
            let worker_cancel = cancel.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::platform_shell::resolve_wsl_symlink_path(&worker_path, &worker_cancel)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(tab_index) = this.tabs.iter().position(|tab| tab.id == tab_id) else {
                    return;
                };
                let tab = &mut this.tabs[tab_index];
                if !tab
                    .wsl_resolve_cancel
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &cancel))
                {
                    return;
                }
                tab.wsl_resolve_cancel = None;
                if tab.load_generation != generation || tab.current_dir != original {
                    return;
                }
                let Ok(resolved) = result else {
                    return;
                };
                if resolved == original || tab_index != this.active {
                    return;
                }
                this.navigate(resolved, cx);
            });
        })
        .detach();
    }

    /// Refresh the cached free-space / volume-name pair for one tab,
    /// off-thread. The underlying `volume_info_for_path` is an NSURL
    /// `resourceValuesForKeys:` round-trip (plus a statfs) that can
    /// stall on network mounts, so render must never call it — it
    /// reads the `Tab::volume_free_bytes` / `Tab::volume_name` cache
    /// this method maintains. Guarded by tab id + load generation so
    /// a slow result for a departed directory is dropped.
    fn refresh_volume_info_in_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        // Safe mode: the statfs / NSURL round-trip is exactly the
        // network-mount hazard the mode exists to rule out. The status
        // bar's free-space segment simply stays empty.
        if crate::safe_mode::enabled() {
            return;
        }
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        let tab_id = tab.id;
        let generation = tab.load_generation;
        let dir = tab.current_dir.clone();
        cx.spawn(async move |this, cx| {
            let info = cx
                .background_executor()
                .spawn(async move { ferail_fs_native::volume_info_for_path(&dir) })
                .await;
            this.update(cx, |this, cx| {
                let Some(idx) = this.tabs.iter().position(|t| t.id == tab_id) else {
                    return;
                };
                if this.tabs[idx].load_generation != generation {
                    return;
                }
                let (free, name, read_only) = match info {
                    Some(v) => (
                        v.available_bytes,
                        Some(SharedString::from(v.name)),
                        v.read_only,
                    ),
                    None => (None, None, false),
                };
                if this.tabs[idx].volume_free_bytes != free
                    || this.tabs[idx].volume_name != name
                    || this.tabs[idx].volume_read_only != read_only
                {
                    this.tabs[idx].volume_free_bytes = free;
                    this.tabs[idx].volume_name = name;
                    this.tabs[idx].volume_read_only = read_only;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-query the cached volume info of every tab. Driven by the
    /// volume watch (mount/unmount/rename) so the status bar's
    /// free-space line tracks live volume changes.
    pub fn refresh_volume_info_all_tabs(&mut self, cx: &mut Context<Self>) {
        for idx in 0..self.tabs.len() {
            self.refresh_volume_info_in_tab(idx, cx);
        }
    }

    fn icon_seeds_from_table_in_tab(&self, idx: usize, cx: &App) -> Vec<(FileEntry, PathBuf)> {
        let Some(tab) = self.tabs.get(idx) else {
            return Vec::new();
        };
        let table = tab.table.read(cx);
        let delegate = table.delegate();
        delegate
            .entries
            .iter()
            .filter_map(|entry| {
                delegate
                    .path_for_entry(entry.id)
                    .map(|path| (entry.clone(), path))
            })
            .collect()
    }

    /// Warm the icon grid's visible entry range: Quick Look thumbnails
    /// for thumbnailable files (background) and crisp NSWorkspace icons
    /// for folders (main thread). Driven from the grid's `uniform_list`
    /// item closure via `App::defer`, which re-runs on every scroll —
    /// so unlike a render-time warm it keeps up as the user scrolls.
    ///
    /// Runs on the Shell entity's context so completion `cx.notify()`s
    /// the Shell, repainting the grid (notifying only the table, as the
    /// list path does, would leave the grid stale since the grid renders
    /// from Shell, not the table).
    fn warm_grid_viewport(
        &mut self,
        entry_range: std::ops::Range<usize>,
        thumb_px: u32,
        icon_px: u32,
        cx: &mut Context<Self>,
    ) {
        if !crate::thumbnails::show_thumbnails(cx) {
            return;
        }
        // Snapshot the folders and thumbnailable files in range.
        let mut folders: Vec<PathBuf> = Vec::new();
        let mut files: Vec<PathBuf> = Vec::new();
        {
            let table = self.active_tab().table.read(cx);
            let del = table.delegate();
            let n = del.entries.len();
            let start = entry_range.start.min(n);
            let end = entry_range.end.min(n);
            for i in start..end {
                let entry = &del.entries[i];
                let Some(path) = del.path_for_entry(entry.id) else {
                    continue;
                };
                if matches!(entry.kind, ferail_core::EntryKind::Directory) {
                    folders.push(path);
                } else if crate::thumbnails::is_thumbnailable(entry) {
                    files.push(path);
                }
            }
        }

        // Folders: the platform icon fetch on the background executor,
        // same as the file thumbnails below. (It used to be a synchronous
        // main-thread NSWorkspace call per uncached folder — one custom
        // folder icon on a sleeping volume stalled the grid.)
        {
            let icons = self.process.icons.borrow();
            let wanted: Vec<(PathBuf, Option<u32>)> = folders
                .iter()
                .filter(|p| icons.needs_path_icon(p, Some(icon_px)))
                .map(|p| (p.clone(), Some(icon_px)))
                .collect();
            drop(icons);
            self.warm_path_icons_async(wanted, cx);
        }

        // Files: Quick Look on the background executor, in bounded
        // concurrent waves rather than the old strictly-serial loop —
        // so a folder of photos fills in parallel instead of one icon
        // at a time.
        //
        // At large icon sizes we also fetch low-res-first: a small
        // `THUMB_PREVIEW_PX` preview before the crisp `thumb_px`, so a
        // soft stand-in paints almost at once and then sharpens (the
        // render side reads `get_best`, which shows the preview until
        // the crisp bucket lands). We only bother when the crisp bucket
        // is the largest one (512) — at smaller buckets a 128-px preview
        // is visually indistinguishable from the final at that slot
        // size, so the extra Quick Look call wouldn't earn its keep;
        // there, parallelism alone carries the speed-up.
        const PREVIEW_PX: u32 = crate::thumbnails::THUMB_PREVIEW_PX;
        const PREVIEW_ABOVE_BUCKET: u32 = 256;
        const WARM_CONCURRENCY: usize = 6;
        let mut work: Vec<(PathBuf, u32)> = Vec::new();
        {
            let cache = self.process.thumbnails.borrow();
            if thumb_px > PREVIEW_ABOVE_BUCKET {
                for path in &files {
                    // A preview is pointless once the crisp size is ready.
                    if cache.get(path, thumb_px).is_none() && cache.needs_fetch(path, PREVIEW_PX) {
                        work.push((path.clone(), PREVIEW_PX));
                    }
                }
            }
            for path in &files {
                if cache.needs_fetch(path, thumb_px) {
                    work.push((path.clone(), thumb_px));
                }
            }
        }
        if work.is_empty() {
            return;
        }
        {
            let mut cache = self.process.thumbnails.borrow_mut();
            for (path, size) in &work {
                cache.mark_in_flight(path.clone(), *size);
            }
        }
        let task_id = self.process.tasks.borrow_mut().begin(
            TaskKind::ThumbnailPrefetch,
            trn!(
                "Loading {n} thumbnail\u{2026}",
                "Loading {n} thumbnails\u{2026}",
                work.len()
            )
            .to_string(),
            false,
        );
        let thumbs = self.process.thumbnails.clone();
        let tasks = self.process.tasks.clone();
        cx.spawn(async move |this, cx| {
            // Each wave spawns up to `WARM_CONCURRENCY` Quick Look calls
            // onto the background pool at once (the pool bounds real
            // parallelism; this cap keeps some threads free for other
            // background work), then drains them, inserting as each lands.
            'outer: for chunk in work.chunks(WARM_CONCURRENCY) {
                let handles: Vec<_> = chunk
                    .iter()
                    .cloned()
                    .map(|(path, size)| {
                        let fetch_path = path.clone();
                        let handle = cx.background_executor().spawn(async move {
                            match crate::video_poster::fetch_content_thumbnail(&fetch_path, size) {
                                crate::video_poster::Fetched::Done(r) => r,
                                // Awaiting yields this pool thread; the
                                // decode runs on the poster worker.
                                crate::video_poster::Fetched::NeedsPoster => {
                                    crate::video_poster::fetch_poster(fetch_path, size).await
                                }
                            }
                        });
                        (path, size, handle)
                    })
                    .collect();
                for (path, size, handle) in handles {
                    let rgba = handle.await;
                    if this
                        .update(cx, |_this, cx| {
                            thumbs.borrow_mut().insert(path, size, rgba);
                            cx.notify();
                        })
                        .is_err()
                    {
                        break 'outer;
                    }
                }
            }
            // Always retire the task; drop it directly if the Shell is gone.
            if this
                .update(cx, |_this, cx| {
                    tasks.borrow_mut().end(task_id);
                    cx.notify();
                })
                .is_err()
            {
                tasks.borrow_mut().end(task_id);
            }
        })
        .detach();
    }

    /// Warm thumbnails for the active tab's currently-visible rows.
    /// Driven by the live `ShowThumbnails` toggle observer so flipping
    /// the setting on fills the viewport immediately rather than on the
    /// next scroll. Cheap to call when thumbnails are off or already
    /// warm — `warm_thumbnails` no-ops in both cases.
    fn warm_active_visible_thumbnails(&mut self, cx: &mut Context<Self>) {
        // The grid warms its own visible range (at its bucket size) from
        // `grid_body` on every render, so this table-range warm only
        // applies in list mode.
        if matches!(self.active_tab().view_mode, crate::grid::ViewMode::Grid) {
            return;
        }
        let table = self.active_tab().table.clone();
        table.update(cx, |ts, cx| {
            let range = ts.visible_range().rows().clone();
            ts.delegate_mut().warm_thumbnails(range, cx);
        });
    }

    /// Warm the just-loaded folder's first screen of thumbnails and file
    /// details.
    ///
    /// The table's `visible_rows_changed` hook only fires when the
    /// visible *row-index range* changes, and the table entity persists
    /// across navigations — so opening a new folder that happens to show
    /// the same range as the previous one (e.g. rows 0..30) never fires
    /// the hook, and thumbnails wouldn't appear until the first scroll.
    /// We warm explicitly here instead. Deferred one frame so the table
    /// has recomputed its real viewport for the new listing; on the very
    /// first load (no layout yet) we fall back to a generous first
    /// screen so thumbnails still appear up front.
    fn warm_loaded_viewport_in_tab(&self, idx: usize, cx: &mut Context<Self>) {
        if idx != self.active {
            return;
        }
        // In grid mode the grid self-warms (at its bucket size) on the
        // render that this load's `cx.notify()` triggers.
        if matches!(self.active_tab().view_mode, crate::grid::ViewMode::Grid) {
            return;
        }
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        let table = tab.table.clone();
        cx.spawn(async move |_this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(16))
                .await;
            table.update(cx, |ts, cx| {
                let mut range = ts.visible_range().rows().clone();
                if range.len() <= 1 {
                    let n = ts.delegate().entries.len().min(48);
                    range = 0..n;
                }
                ts.delegate_mut().warm_visible_details(range.clone(), cx);
                ts.delegate_mut().warm_thumbnails(range, cx);
            });
        })
        .detach();
    }

    fn start_icon_warm(&self, seeds: Vec<(FileEntry, PathBuf)>, cx: &mut Context<Self>) {
        if seeds.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            for chunk in seeds.chunks(ICON_WARM_CHUNK) {
                cx.background_executor().timer(ICON_WARM_INTERVAL).await;
                let rows = chunk.to_vec();
                if this
                    .update(cx, |this, cx| {
                        let mut icons = this.process.icons.borrow_mut();
                        for (entry, path) in &rows {
                            let _ = icons.icon_for(entry, path);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Warm path-keyed sidebar icons (tree rows, favorites) in the
    /// background. The render path never fetches — `folder_icon_for`
    /// returns the blank placeholder under the render guard — so
    /// without this, rows revealed by expanding a folder kept their
    /// blank icon until some unrelated file-list warm happened to
    /// cache the same path. `Shell::render` collects the paths that
    /// `needs_path_icon` says are neither cached nor in flight each
    /// frame; a failed fetch caches the blank placeholder, so this
    /// converges instead of respawning.
    fn start_tree_icon_warm(&self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let items = paths.into_iter().map(|p| (p, None)).collect();
        self.warm_path_icons_async(items, cx);
    }

    /// Fetch path-keyed icons (sidebar tree rows at the small size, grid
    /// folders at a crisp bucket) on the background executor and land
    /// them in the shared `IconCache`, repainting once per wave. The
    /// platform fetch (`fetch_icon_rgba`) can block on a sleeping volume
    /// — a folder with custom artwork makes NSWorkspace stat it — so it
    /// must never run on the UI thread (Prime Directive). Callers pass
    /// only paths `needs_path_icon` approved; this marks them in flight
    /// so the per-frame collectors don't re-request a pending fetch.
    fn warm_path_icons_async(&self, items: Vec<(PathBuf, Option<u32>)>, cx: &mut Context<Self>) {
        if items.is_empty() {
            return;
        }
        {
            let mut icons = self.process.icons.borrow_mut();
            for (path, size) in &items {
                icons.mark_path_icon_in_flight(path, *size);
            }
        }
        let task_id = self.process.tasks.borrow_mut().begin(
            TaskKind::IconPrefetch,
            trn!(
                "Loading {n} icon\u{2026}",
                "Loading {n} icons\u{2026}",
                items.len()
            )
            .to_string(),
            false,
        );
        let icons = self.process.icons.clone();
        let tasks = self.process.tasks.clone();
        cx.spawn(async move |this, cx| {
            // Bounded waves, like the grid's thumbnail warm: a few fetches
            // in parallel, land the wave, repaint, next wave.
            for chunk in items.chunks(ICON_WARM_CHUNK) {
                let handles: Vec<_> = chunk
                    .iter()
                    .cloned()
                    .map(|(path, size)| {
                        cx.background_executor().spawn(async move {
                            let px = crate::icons::IconCache::path_icon_px(size);
                            let fetched = ferail_fs_native::fetch_icon_rgba(&path, px);
                            (path, size, fetched)
                        })
                    })
                    .collect();
                let mut landed = Vec::with_capacity(handles.len());
                for handle in handles {
                    landed.push(handle.await);
                }
                let alive = this
                    .update(cx, |_, cx| {
                        {
                            let mut icons = icons.borrow_mut();
                            for (path, size, fetched) in landed {
                                icons.insert_path_icon(&path, size, fetched);
                            }
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
            tasks.borrow_mut().end(task_id);
        })
        .detach();
    }

    fn refresh_active_tab_heats(&mut self, cx: &mut Context<Self>) {
        let table = self.active_tab().table.clone();
        table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            let heats: Vec<f32> = delegate
                .entries
                .iter()
                .map(|entry| self.ant_heat(entry.id))
                .collect();
            delegate.heats = heats;
            // Under an Ant Trail sort the heat *is* the sort key, so new
            // heat means a new row order — hydration finishing (or a
            // fresh visit) has to re-rank, not just re-tint.
            if delegate.current_sort.map(|(col, _)| col)
                == Some(crate::file_list::SortColumn::AntTrail)
            {
                delegate.apply_effective_sort();
            }
            state.refresh(cx);
        });
    }

    fn start_metadata_load(&mut self, cx: &mut Context<Self>) {
        if self.process.metadata_loaded.replace(true) {
            self.favorites_section_collapsed = self.process.favorites_section_collapsed.get();
            self.refresh_active_tab_heats(cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let db = open_metadata_db();
                    // Once-per-launch cache pruning: the files /
                    // folder_sizes / folder_usage tables otherwise grow
                    // without bound (dead paths kept forever;
                    // folder_usage is loaded wholesale into RAM below).
                    if let Some(d) = db.as_ref() {
                        if let Ok(g) = d.lock() {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs() as i64)
                                .unwrap_or(0);
                            if let Err(e) = g.prune_stale(now) {
                                crate::log_warn!(90, "metadata prune failed: {e}");
                            }
                            // Heal the magic cache across sniffer
                            // upgrades: labels cached by an older
                            // detector otherwise shadow the improved
                            // answer forever (rows only invalidate on
                            // file mtime, and files don't change when
                            // Ferail does). One UPDATE, once per
                            // `MAGIC_REVISION` bump; rows re-sniff
                            // lazily as folders are browsed.
                            let rev = ferail_fs_native::MAGIC_REVISION.to_string();
                            let stored = g.get_preference("magic_revision").ok().flatten();
                            if stored.as_deref() != Some(rev.as_str()) {
                                match g.reset(ferail_meta::ResetScope::Magic) {
                                    Ok(()) => {
                                        if let Err(e) = g.set_preference("magic_revision", &rev) {
                                            crate::log_warn!(
                                                90,
                                                "magic revision stamp failed: {e}"
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        crate::log_warn!(90, "magic cache heal failed: {e}")
                                    }
                                }
                            }
                        }
                    }
                    let (ant_visits, ant_max, recents) = hydrate_ant_trail(db.as_ref());
                    let favs_collapsed = db
                        .as_ref()
                        .and_then(|d| d.lock().ok().map(|g| g.favorites_section_collapsed()))
                        .unwrap_or(false);
                    // `None` = the DB exists but the favorites load
                    // FAILED (busy, poisoned lock, corrupt row) —
                    // which is a different thing from "the user has no
                    // favorites". Conflating them is a data-loss
                    // hazard: hydrating empty with a writable DB
                    // attached lets the first reorder replace_all the
                    // real on-disk list with nothing.
                    let favorites: Option<Vec<ferail_core::favorites::Favorite>> =
                        match db.as_ref() {
                            // No DB at all (e.g. no $HOME): an empty
                            // in-memory list is honest.
                            None => Some(Vec::new()),
                            Some(d) => d.lock().ok().and_then(|g| g.load_favorites().ok()),
                        }
                        .map(|favs| {
                            favs.into_iter()
                                .map(|mut fav| {
                                    // Path-identity contract: persisted paths
                                    // re-enter the app here, so re-canonicalize
                                    // (already on the background executor; a
                                    // missing target falls through unchanged
                                    // and keeps its Missing-state handling).
                                    if let ferail_core::favorites::FavoriteTarget::Path(p) =
                                        fav.target
                                    {
                                        fav.target = ferail_core::favorites::FavoriteTarget::Path(
                                            path::canonicalize_for_identity(p),
                                        );
                                    }
                                    fav
                                })
                                .collect()
                        });
                    (db, ant_visits, ant_max, favs_collapsed, favorites, recents)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let (db, ant_visits, ant_max, favs_collapsed, favorites, recents) = loaded;
                *this.process.metadata_db.borrow_mut() = db.clone();
                *this.process.ant_visits.borrow_mut() = ant_visits;
                this.process.ant_max.set(ant_max);
                // Merge the DB's recency-ordered list *behind* whatever
                // this session already navigated to before the async
                // load landed — the live entries stay most-recent,
                // historical ones fill in behind, deduped and capped.
                {
                    let mut live = this.process.recents.borrow_mut();
                    for p in recents {
                        if !live.contains(&p) {
                            live.push(p);
                        }
                    }
                    live.truncate(crate::process_state::RECENTS_CAP);
                }
                this.process.favorites_section_collapsed.set(favs_collapsed);
                this.favorites_section_collapsed = favs_collapsed;
                // Attach the writable DB to the favorites entity and
                // hydrate. The dev seed runs only when the entry list
                // is empty AND `FERAIL_DEV_SEED_FAVORITES=1` — see
                // `crate::favorites::maybe_seed_dev_favorites`.
                let fav_entity = this.process.favorites().clone();
                fav_entity.update(cx, |f, cx| match favorites {
                    Some(favorites) => {
                        if let Some(d) = db.clone() {
                            f.attach_db(d);
                        }
                        f.hydrate(favorites, cx);
                        crate::favorites::maybe_seed_dev_favorites(f, cx);
                    }
                    None => {
                        // Load failed: run this session's favorites
                        // in-memory only. Without an attached DB every
                        // persist helper no-ops, so the unreadable
                        // on-disk list can't be clobbered by an empty
                        // replace_all; the next launch retries.
                        crate::log_warn!(
                            90,
                            "favorites load failed; running detached (no persistence this session)"
                        );
                        f.hydrate(Vec::new(), cx);
                    }
                });
                this.refresh_active_tab_heats(cx);
                // Folder-size passes kicked before this point ran
                // cache-blind (metadata_db was still None) and
                // couldn't persist what they computed. One re-kick
                // with the DB attached makes the cache durable.
                this.restart_folder_size_passes(false, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Re-run the folder-size pass for every tab. Cancels any pass
    /// still in flight first. Both callers pass `force = false` — the
    /// metadata DB attaching and the window returning from the
    /// background are both "re-seed from the cache" moments, not
    /// "re-measure everything" ones. The parameter stays because the
    /// pass itself supports forcing; only Refresh arms it today, per-tab
    /// through `Tab::force_folder_sizes`.
    fn restart_folder_size_passes(&mut self, force: bool, cx: &mut Context<Self>) {
        // Respect the Performance toggle — this is also the activation
        // re-seed path (`observe_window_activation`), so disabling folder
        // sizing stops the re-scan that would otherwise fire every time
        // the window comes forward.
        if !crate::folder_sizes::folder_sizing_enabled(cx) {
            return;
        }
        let db = self.process.db_snapshot();
        if db.is_none() {
            return;
        }
        let fs = self.process.fs.clone();
        let tasks = self.process.tasks.clone();
        for idx in 0..self.tabs.len() {
            if let Some(cancel) = self.tabs[idx].folder_size_cancel.take() {
                cancel.store(true, Ordering::Relaxed);
            }
            let cancel = Arc::new(AtomicBool::new(false));
            self.tabs[idx].folder_size_cancel = Some(cancel.clone());
            crate::folder_sizes::start(
                self.tabs[idx].table.clone(),
                fs.clone(),
                db.clone(),
                tasks.clone(),
                self.tabs[idx].id,
                self.tabs[idx].load_generation,
                cancel,
                force,
                cx,
            );
        }
    }

    pub(crate) fn reload_tabs_matching_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let targets: Vec<(TabId, PathBuf)> = self
            .tabs
            .iter()
            // A tab showing a tool result is not
            // displaying its directory — a watcher reload would clobber
            // the results.
            .filter(|tab| tab.tool_result.is_none())
            .filter(|tab| paths.iter().any(|path| path == &tab.current_dir))
            .map(|tab| (tab.id, tab.current_dir.clone()))
            .collect();
        for (tab_id, path) in targets {
            self.load_path_for_tab(tab_id, path, cx);
        }
    }

    /// Reload every directory-displaying tab in this window. Used on
    /// wake-from-sleep (docs/features/POWER.md): contents may have
    /// changed while the watcher was asleep, and the cheapest correct
    /// answer is a re-list. Skips tool results
    /// for the same reason `reload_tabs_matching_paths` does — a reload
    /// would clobber the results.
    pub(crate) fn reload_dir_tabs(&mut self, cx: &mut Context<Self>) {
        let targets: Vec<(TabId, PathBuf)> = self
            .tabs
            .iter()
            .filter(|tab| tab.tool_result.is_none())
            .map(|tab| (tab.id, tab.current_dir.clone()))
            .collect();
        for (tab_id, path) in targets {
            self.load_path_for_tab(tab_id, path, cx);
        }
    }

    pub(crate) fn broadcast_reload_for_process(
        process: &Rc<crate::process_state::ProcessState>,
        paths: Vec<PathBuf>,
        cx: &mut AsyncApp,
    ) {
        if paths.is_empty() {
            return;
        }
        Self::invalidate_folder_size_ancestors(process, &paths, cx);
        for weak in process.live_shells() {
            if let Some(shell) = weak.upgrade() {
                let paths = paths.clone();
                shell.update(cx, |this, cx| {
                    this.reload_tabs_matching_paths(&paths, cx);
                });
            }
        }
        // A watched directory changed — a favorited path inside it may
        // have been deleted/moved/restored. Re-check availability off the
        // UI thread (no-op when there are no path favorites). This is the
        // live Missing-transition hook (§8): favorite parents are watched
        // independently of the visible tabs (`watch_favorite_dirs`).
        process
            .favorites()
            .update(cx, |f, cx| f.refresh_availability(cx));
    }

    /// Drop cached folder sizes for each changed directory and all of
    /// its ancestors. A change at `P` alters the recursive size of `P`
    /// and every directory above it (each contains the change), yet
    /// only `P`'s own mtime moves — the ancestors' rows would read
    /// stale until their TTL lapses (see folder_sizes.rs). This is the
    /// exact, immediate path for *in-app* work and for external
    /// shallow changes the watcher catches; the deletes run off the UI
    /// thread and are single-row primary-key hits.
    fn invalidate_folder_size_ancestors(
        process: &Rc<crate::process_state::ProcessState>,
        paths: &[PathBuf],
        cx: &mut AsyncApp,
    ) {
        let Some(db) = process.db_snapshot() else {
            return;
        };
        // Normalize to the same key shape the cache stores
        // (`normalize_path_key`, matching folder_sizes seeds).
        let keys: Vec<PathBuf> = paths
            .iter()
            .map(|p| ferail_core::node_store::normalize_path_key(p))
            .collect();
        cx.background_executor()
            .spawn(async move {
                if let Ok(guard) = db.lock() {
                    for key in &keys {
                        for ancestor in key.ancestors() {
                            let _ = guard.delete_folder_size(&ancestor.to_string_lossy());
                        }
                    }
                }
            })
            .detach();
    }

    // The file-op spawner genuinely needs each of these inputs.
    #[allow(clippy::too_many_arguments)]
    fn spawn_file_op(
        &mut self,
        reload_path: PathBuf,
        op: impl FnOnce() -> Result<Vec<PathBuf>, String> + Send + 'static,
        failure_label: &'static str,
        task_label: Option<String>,
        success_toast: FileOpSuccessToast,
        undo: FileOpUndo,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let process = self.process.clone();
        let win = window.window_handle();
        let weak = cx.weak_entity();
        let task_id = task_label.map(|label| {
            self.process
                .tasks
                .borrow_mut()
                .begin(crate::tasks::TaskKind::FileOp, label, false)
        });
        cx.spawn(async move |_this, cx| {
            let result = cx.background_executor().spawn(async move { op() }).await;
            let created = result.as_ref().ok().cloned().unwrap_or_default();
            let error = result.as_ref().err().cloned();
            let surfaced = if let Some(shell) = weak.upgrade() {
                let created_for_undo = created.clone();
                shell.update(cx, move |this, cx| {
                    let surfaced = task_id
                        .map(|id| {
                            if let Some(message) = error.as_ref() {
                                this.process
                                    .tasks
                                    .borrow_mut()
                                    .end_failed(id, message.clone());
                                false
                            } else {
                                this.process.tasks.borrow_mut().end_and_was_surfaced(id)
                            }
                        })
                        .unwrap_or(false);
                    if error.is_none() {
                        // If the user is still looking at the folder the op
                        // produced its results in, select them (renamed /
                        // new / duplicated / aliased entries) and bring the
                        // first into view once the reload lands. Results in
                        // other folders (e.g. an alias dropped into a
                        // subfolder) are filtered out by the parent check.
                        let dir = this.active_tab().current_dir.clone();
                        let names = created_for_undo
                            .iter()
                            .filter(|p| p.parent() == Some(dir.as_path()))
                            .filter_map(|p| p.file_name())
                            .map(|n| n.to_string_lossy().into_owned())
                            .collect();
                        this.queue_select_names_if_current(&dir, names);
                        undo.push(this, created_for_undo);
                    }
                    cx.notify();
                    surfaced
                })
            } else {
                false
            };
            match result {
                Ok(_) => {
                    Shell::broadcast_reload_for_process(&process, vec![reload_path], cx);
                    if let FileOpSuccessToast::IfSurfaced(message) = success_toast {
                        if surfaced {
                            let _ = win.update(cx, |_, window, cx| {
                                use gpui_component::notification::Notification;
                                window.push_notification(Notification::success(message), cx);
                            });
                        }
                    }
                }
                Err(e) => {
                    crate::log_warn!(90, "{failure_label} failed: {e}");
                    let _ = win.update(cx, |_, window, cx| {
                        window.push_notification(
                            file_op_error_notification(&crate::i18n::tr_static(failure_label), &e),
                            cx,
                        );
                    });
                }
            }
        })
        .detach();
    }

    /// Append a reversible op to the undo stack, evicting the oldest
    /// entry when capacity is exceeded.
    pub(crate) fn push_undo(&mut self, op: UndoOp) {
        self.process.push_undo(op, UNDO_STACK_CAP);
    }

    pub fn on_undo_last_action(
        &mut self,
        _: &UndoLastAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Undo");
        use gpui_component::notification::Notification;
        let Some(op) = self.process.undo_stack.borrow_mut().pop_back() else {
            window.push_notification(Notification::info(tr!("Nothing to undo")), cx);
            return;
        };
        let label = op.label();
        match op {
            UndoOp::AddFavorite(id) => {
                self.process.favorites().update(cx, |f, cx| {
                    f.remove(id, cx);
                });
                window.push_notification(Notification::success(label.clone()), cx);
            }
            UndoOp::RemoveFavorite(fav) => {
                self.process.favorites().update(cx, |f, cx| {
                    f.restore(fav, cx);
                });
                window.push_notification(Notification::success(label.clone()), cx);
            }
            fs_op => {
                // Prime Directive: `apply_fs` is an arbitrary-size
                // filesystem mutation (RemoveCreated of a whole pasted
                // tree is a recursive delete). Run it on the background
                // executor behind a task-registry row; the handler
                // returns immediately and the toast lands on
                // completion.
                let process = self.process.clone();
                let mut reload = fs_op.affected_parents();
                if reload.is_empty() {
                    reload.push(self.active_tab().current_dir.clone());
                }
                let win = window.window_handle();
                let task_id = self.process.tasks.borrow_mut().begin(
                    crate::tasks::TaskKind::FileOp,
                    label.to_string(),
                    false,
                );
                cx.spawn(async move |_this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { fs_op.apply_fs() })
                        .await;
                    cx.update(|_| {
                        let mut tasks = process.tasks.borrow_mut();
                        match &result {
                            Ok(()) => tasks.end(task_id),
                            Err(e) => tasks.end_failed(task_id, e.clone()),
                        }
                    });
                    match result {
                        Ok(()) => {
                            Shell::broadcast_reload_for_process(&process, reload, cx);
                            let _ = win.update(cx, |_, window, cx| {
                                window.push_notification(Notification::success(label.clone()), cx);
                            });
                        }
                        Err(e) => {
                            let _ = win.update(cx, |_, window, cx| {
                                window.push_notification(
                                    Notification::error(tr!("Undo failed: {detail}", detail = e)),
                                    cx,
                                );
                            });
                        }
                    }
                })
                .detach();
            }
        }
    }

    pub fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        if self
            .active_tab()
            .tool_result
            .as_ref()
            .is_some_and(|surface| surface.flat_mode().is_some())
        {
            let tab_id = self.active_tab().id;
            self.restart_flat_view(tab_id, None, cx);
            return;
        }
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
    }

    // ----- Favorites mutations (iter 4) ---------------------------

    /// Cmd+D / context-menu / menu-bar toggle. Reads the target from
    /// `favorites_context_path` (set by every "Add to Favorites" /
    /// "Remove from Favorites" closure), falling back to the file-list
    /// selection's path, then to the active tab's `current_dir`.
    /// Files are rejected with a toast (§2.3).
    pub fn on_toggle_favorite_for_target(
        &mut self,
        _: &ToggleFavoriteForTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        let target = self.resolve_favorite_target(cx);
        let Some((path, kind)) = target else {
            window.push_notification(
                Notification::info(tr!("No folder available to add to Favorites.")),
                cx,
            );
            return;
        };
        match kind {
            FavoriteResolved::Folder => {
                // Path-identity boundary + prime directive: the
                // canonicalize stat runs on a worker; the toggle
                // applies back on the main thread with the window
                // still available for the notification.
                cx.spawn_in(window, async move |this, cx| {
                    let canonical = cx
                        .background_executor()
                        .spawn(async move { path::canonicalize_for_identity(path) })
                        .await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.apply_toggle_favorite_canonical(canonical, window, cx);
                    });
                })
                .detach();
            }
            FavoriteResolved::NotAFolder => {
                window.push_notification(
                    Notification::info(tr!("Only folders can be added to Favorites.")),
                    cx,
                );
            }
        }
    }

    /// Second half of `on_toggle_favorite_for_target`, after the
    /// background canonicalize: add or remove `canonical` from
    /// Favorites with undo + notification. Main thread, no I/O.
    fn apply_toggle_favorite_canonical(
        &mut self,
        canonical: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        let already = self.process.favorites().read(cx).contains_path(&canonical);
        let favs = self.process.favorites().clone();
        if already {
            let id = self
                .process
                .favorites()
                .read(cx)
                .id_for_path(&canonical)
                .expect("contains_path returned true");
            let label = self
                .process
                .favorites()
                .read(cx)
                .entry_by_id(id)
                .map(|f| f.effective_label())
                .unwrap_or_else(|| tr!("favorite").to_string());
            // Capture the full entry before removal so the undo
            // restores name + icon + sort_index + date_added.
            let removed_for_undo = self.process.favorites().read(cx).entry_by_id(id).cloned();
            self.remove_favorite_collapsing(id, cx);
            if let Some(fav) = removed_for_undo {
                self.push_undo(UndoOp::RemoveFavorite(fav));
            }
            window.push_notification(
                Notification::info(tr!(
                    "Removed \u{201C}{label}\u{201D} from Favorites · Cmd+Z to undo",
                    label = label
                )),
                cx,
            );
        } else {
            let label = canonical
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| canonical.to_string_lossy().into_owned());
            let added_id = favs.update(cx, |f, cx| {
                match f.add_path(
                    canonical.clone(),
                    ferail_core::favorites::FavoriteKind::Folder,
                    cx,
                ) {
                    crate::favorites::AddOutcome::Added(id) => Some(id),
                    crate::favorites::AddOutcome::Existing(_) => None,
                }
            });
            if let Some(id) = added_id {
                self.push_undo(UndoOp::AddFavorite(id));
            }
            window.push_notification(
                Notification::success(tr!(
                    "Added \u{201C}{label}\u{201D} to Favorites",
                    label = label
                )),
                cx,
            );
        }
    }

    /// Remove a favorite with the §3.2 collapse-on-remove animation.
    /// The row is marked `removing` — it fades and collapses in place —
    /// and dropped from the entity once the fade window elapses. Callers
    /// capture the pre-removal entry for undo *before* calling this, so a
    /// Cmd+Z during the brief collapse still restores name/icon/sort.
    fn remove_favorite_collapsing(
        &mut self,
        id: ferail_core::favorites::FavoriteId,
        cx: &mut Context<Self>,
    ) {
        // Must match `favorites_section::COLLAPSE_MS`.
        const FAV_COLLAPSE_MS: u64 = 150;
        if !self.fav_removing.insert(id) {
            return; // already collapsing
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(FAV_COLLAPSE_MS))
                .await;
            let _ = this.update(cx, |this, cx| {
                // Still marked removing ⇒ the fade finished normally; drop
                // it now. If it was already cleared (undo, re-add), the
                // entity remove would be redundant, so skip it.
                if this.fav_removing.remove(&id) {
                    this.process.favorites().update(cx, |f, cx| {
                        f.remove(id, cx);
                    });
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Backs `File → Add to Favorites` and the section-header `+`
    /// button. No-op if the current folder is already a favorite
    /// (dedup pulse is emitted by the entity).
    pub fn on_add_current_folder_to_favorites(
        &mut self,
        _: &AddCurrentFolderToFavorites,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self.active_tab().current_dir.clone();
        self.favorites_context_path = Some(path);
        self.on_toggle_favorite_for_target(&ToggleFavoriteForTarget, window, cx);
    }

    pub fn on_toggle_favorites_section(
        &mut self,
        _: &ToggleFavoritesSection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_favorites_section_collapsed(cx);
    }

    // ----- Favorites: one-shot sorts (§4.5) ------------------------

    pub fn on_sort_favorites_by_name(
        &mut self,
        _: &SortFavoritesByName,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.process.favorites().update(cx, |f, cx| {
            f.one_shot_sort(ferail_core::favorites::FavoriteSort::NameAsc, cx);
        });
    }

    pub fn on_sort_favorites_by_date_added_newest(
        &mut self,
        _: &SortFavoritesByDateAddedNewest,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.process.favorites().update(cx, |f, cx| {
            f.one_shot_sort(ferail_core::favorites::FavoriteSort::DateAddedNewest, cx);
        });
    }

    pub fn on_sort_favorites_by_date_added_oldest(
        &mut self,
        _: &SortFavoritesByDateAddedOldest,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.process.favorites().update(cx, |f, cx| {
            f.one_shot_sort(ferail_core::favorites::FavoriteSort::DateAddedOldest, cx);
        });
    }

    pub fn on_sort_favorites_by_kind(
        &mut self,
        _: &SortFavoritesByKind,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.process.favorites().update(cx, |f, cx| {
            f.one_shot_sort(ferail_core::favorites::FavoriteSort::Kind, cx);
        });
    }

    // ----- Favorites: keyboard reorder (§4.4) ----------------------

    pub fn on_move_favorite_up(
        &mut self,
        _: &MoveFavoriteUp,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.focused_favorite {
            self.process
                .favorites()
                .update(cx, |f, cx| f.shift(id, -1, cx));
        }
    }

    pub fn on_move_favorite_down(
        &mut self,
        _: &MoveFavoriteDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.focused_favorite {
            self.process
                .favorites()
                .update(cx, |f, cx| f.shift(id, 1, cx));
        }
    }

    // ----- Favorites: rename + custom icons (iter 9 / §6 / §7) -----

    /// Resolve the favorite id for the next rename/icon action. The
    /// row-level context menu sets `favorites_context_path` before
    /// dispatching. Rename/icon actions only exist on favorite rows,
    /// and favorite paths are canonicalized at every entry boundary
    /// (hydrate, add, external drop) — so the lookup needs no stat
    /// here, which matters: this runs in a menu-dispatch handler on
    /// the UI thread.
    fn pop_favorite_id_for_action(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<ferail_core::favorites::FavoriteId> {
        let path = self.favorites_context_path.take()?;
        self.process.favorites().read(cx).id_for_path(&path)
    }

    pub fn on_rename_favorite(
        &mut self,
        _: &RenameFavorite,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.pop_favorite_id_for_action(cx) else {
            return;
        };
        let current = self
            .process
            .favorites()
            .read(cx)
            .entry_by_id(id)
            .map(|f| f.effective_label())
            .unwrap_or_default();
        // Same gpui rename modal the file list uses (renaming the
        // shortcut's label, not the folder on disk) — consistent
        // surface and cross-platform, unlike the old native prompt.
        // A favorite's label is not a filesystem name, so skip filename
        // validation (it may legitimately contain `:`, trailing dots, etc.).
        self.open_named_prompt(
            tr!("Rename Favorite"),
            tr!("New name"),
            current,
            false,
            move |this, new_name, _window, cx| {
                this.process
                    .favorites()
                    .update(cx, |f, cx| f.rename(id, Some(new_name), cx));
            },
            window,
            cx,
        );
    }

    pub fn on_reset_favorite_name(
        &mut self,
        _: &ResetFavoriteName,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.pop_favorite_id_for_action(cx) else {
            return;
        };
        self.process
            .favorites()
            .update(cx, |f, cx| f.rename(id, None, cx));
    }

    pub fn on_reset_favorite_icon(
        &mut self,
        _: &ResetFavoriteIcon,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.pop_favorite_id_for_action(cx) else {
            return;
        };
        self.process
            .favorites()
            .update(cx, |f, cx| f.set_icon(id, None, cx));
    }

    /// Context-menu "Locate…" (§8.3): repoint the favorite under
    /// `favorites_context_path` at a user-chosen folder, keeping its
    /// id / display_name / sort_index. Also reachable from the broken-
    /// state dialog ([`Self::show_missing_favorite_dialog`]).
    pub fn on_locate_favorite(
        &mut self,
        _: &LocateFavorite,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.pop_favorite_id_for_action(cx) else {
            return;
        };
        self.locate_favorite(id, cx);
    }

    /// Present the native folder picker and, on a choice, repoint
    /// favorite `id` at the chosen folder. The picker is a synchronous
    /// native modal (like the rename prompt); the chosen path is
    /// canonicalized on a worker before the repoint so favorites keep
    /// their canonical-path identity (Prime Directive: no stat on the
    /// dispatch thread).
    pub(crate) fn locate_favorite(
        &mut self,
        id: ferail_core::favorites::FavoriteId,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            // `pick_folder` is a blocking native modal (`NSOpenPanel::
            // runModal`) that spins a *nested* run loop. It MUST run with
            // no GPUI `App` borrow held: the nested loop keeps servicing
            // the foreground executor, so any pending task (folder sizes,
            // thumbnail warms, watchers) fires mid-modal and calls
            // `AsyncApp::update` → `App::borrow_mut`. Calling the picker
            // from inside the action handler (App already borrowed by the
            // `Context`) makes that reentrant borrow panic with "RefCell
            // already borrowed". The spawned task body runs between
            // updates, so the borrow is free here.
            let Some(chosen) = crate::platform_shell::pick_folder() else {
                return;
            };
            let canonical = cx
                .background_executor()
                .spawn(async move { path::canonicalize_for_identity(chosen) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.process.favorites().update(cx, |f, cx| {
                    f.repoint(
                        id,
                        ferail_core::favorites::FavoriteTarget::Path(canonical),
                        cx,
                    );
                });
            });
        })
        .detach();
    }

    /// §8.2 broken-target dialog. Shown when an `Unmounted` / `Missing`
    /// favorite is clicked: offers **Locate…** (repoint), **Remove from
    /// Favorites** (with undo), and **Keep** (dismiss, leave it broken).
    /// Replaces the old single-button NSAlert.
    pub(crate) fn show_missing_favorite_dialog(
        &mut self,
        id: ferail_core::favorites::FavoriteId,
        path: PathBuf,
        state: FavoriteState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::dialog::DialogFooter;
        use gpui_component::notification::Notification;
        let shell = cx.entity();
        let (title, body) = match state {
            FavoriteState::Unmounted => (
                tr!("Volume not mounted"),
                tr!(
                    "\u{201C}{path}\u{201D} isn\u{2019}t currently mounted. Locate it on a mounted volume, or remove the shortcut.",
                    path = path.display()
                ),
            ),
            _ => (
                tr!("Favorite can\u{2019}t be found"),
                tr!(
                    "\u{201C}{path}\u{201D} may have been moved or deleted. Locate it to repoint this shortcut, or remove it from Favorites.",
                    path = path.display()
                ),
            ),
        };
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let shell_locate = shell.clone();
            let shell_remove = shell.clone();
            let title = title.clone();
            let body = body.clone();
            dialog
                .title(title)
                .child(div().text_scale_sm().child(body))
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("fav-missing-keep")
                                .label(tr!("Keep"))
                                .small()
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("fav-missing-remove")
                                .label(tr!("Remove from Favorites"))
                                .danger()
                                .small()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let removed = shell_remove
                                        .read(cx)
                                        .process
                                        .favorites()
                                        .read(cx)
                                        .entry_by_id(id)
                                        .cloned();
                                    let label = removed
                                        .as_ref()
                                        .map(|f| f.effective_label())
                                        .unwrap_or_else(|| tr!("favorite").to_string());
                                    shell_remove.update(cx, |s, cx| {
                                        s.remove_favorite_collapsing(id, cx);
                                        if let Some(fav) = removed {
                                            s.push_undo(UndoOp::RemoveFavorite(fav));
                                        }
                                    });
                                    window.push_notification(
                                        Notification::info(tr!(
                                            "Removed \u{201C}{label}\u{201D} from Favorites \u{00B7} Cmd+Z to undo",
                                            label = label
                                        )),
                                        cx,
                                    );
                                }),
                        )
                        .child(
                            Button::new("fav-missing-locate")
                                .label(tr!("Locate\u{2026}"))
                                .primary()
                                .small()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    shell_locate.update(cx, |s, cx| {
                                        s.locate_favorite(id, cx);
                                    });
                                }),
                        ),
                )
        });
    }

    // ----- Favorites: keyboard focus + delete (§11.4) --------------

    /// Move keyboard focus to the previous (`by < 0`) or next favorite
    /// row, wrapping at the ends. With nothing focused yet, lands on the
    /// first (next) or last (previous) entry so a single arrow press
    /// from the section header enters the list.
    fn move_favorite_focus(&mut self, by: isize, cx: &mut Context<Self>) {
        let entries = self.process.favorites().read(cx).entries().to_vec();
        if entries.is_empty() {
            return;
        }
        let next = match self
            .focused_favorite
            .and_then(|id| entries.iter().position(|f| f.id == id))
        {
            Some(pos) => {
                let len = entries.len() as isize;
                ((pos as isize + by).rem_euclid(len)) as usize
            }
            None if by < 0 => entries.len() - 1,
            None => 0,
        };
        self.focused_favorite = Some(entries[next].id);
        cx.notify();
    }

    pub fn on_focus_favorite_up(
        &mut self,
        _: &FocusFavoriteUp,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_favorite_focus(-1, cx);
    }

    pub fn on_focus_favorite_down(
        &mut self,
        _: &FocusFavoriteDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_favorite_focus(1, cx);
    }

    /// Enter on the focused favorite — navigate the active tab to it
    /// when it's `Available`, else surface the broken-target dialog.
    pub fn on_activate_favorite(
        &mut self,
        _: &ActivateFavorite,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.focused_favorite else {
            return;
        };
        let Some(fav) = self.process.favorites().read(cx).entry_by_id(id).cloned() else {
            return;
        };
        let ferail_core::favorites::FavoriteTarget::Path(path) = fav.target else {
            return;
        };
        match self.process.favorites().read(cx).state_for(id) {
            FavoriteState::Available => self.navigate_from_favorite(path, cx),
            other => self.show_missing_favorite_dialog(id, path, other, window, cx),
        }
    }

    /// Delete / Backspace on the focused favorite — remove it with undo,
    /// the keyboard twin of the context-menu / source-folder removes.
    pub fn on_delete_favorite(
        &mut self,
        _: &DeleteFavorite,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        let Some(id) = self.focused_favorite else {
            return;
        };
        let removed = self.process.favorites().read(cx).entry_by_id(id).cloned();
        let Some(fav) = removed else {
            // Stale focus (entry already gone) — clear it.
            self.focused_favorite = None;
            return;
        };
        let label = fav.effective_label();
        self.remove_favorite_collapsing(id, cx);
        self.push_undo(UndoOp::RemoveFavorite(fav));
        self.focused_favorite = None;
        window.push_notification(
            Notification::info(tr!(
                "Removed \u{201C}{label}\u{201D} from Favorites \u{00B7} Cmd+Z to undo",
                label = label
            )),
            cx,
        );
    }

    pub fn on_open_favorite_icon_picker(
        &mut self,
        _: &OpenFavoriteIconPicker,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Resolve the contextual favorite up front (same path Rename /
        // Reset Icon use), then hand the shared favorites entity + id to
        // the picker window. The picker writes the chosen glyph straight
        // through the entity — no `favorites_context_path` to keep alive
        // across the window's lifetime.
        let Some(id) = self.pop_favorite_id_for_action(cx) else {
            return;
        };
        let favorites = self.process.favorites().clone();
        crate::favorite_icon_picker::open_window(cx, favorites, id);
    }

    /// Pick the path the next favorites mutation should target.
    /// Order of precedence:
    ///   1. `favorites_context_path` (set by sidebar / breadcrumb /
    ///      favorite-row context menus before dispatching the action)
    ///   2. The file-list row most recently right-clicked or selected
    ///      via [`Shell::target_row`].
    ///   3. The active tab's `current_dir` (so a keyboard Cmd+D with
    ///      nothing selected toggles the current folder).
    fn resolve_favorite_target(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(PathBuf, FavoriteResolved)> {
        // A path that is already in the favorites index must be
        // classifiable as `Folder` even when its on-disk presence is
        // gone (Missing or Unmounted state) — otherwise "Remove from
        // Favorites" on a broken row routes to the NotAFolder rejection
        // toast and the user can never remove the stale shortcut.
        let already_favorite =
            |path: &Path, this: &Self| this.process.favorites().read(cx).contains_path(path);
        if let Some(p) = self.favorites_context_path.take() {
            // Every surface that sets `favorites_context_path` is
            // folder-only by construction (favorites rows, sidebar
            // tree rows, breadcrumb segments, the active tab's
            // current_dir) — classify as Folder without a stat. An
            // `is_dir()` here hung Cmd+D on dead network mounts
            // (Prime Directive). Keep that invariant when adding
            // setters.
            return Some((p, FavoriteResolved::Folder));
        }
        if let Some(row) = self.target_row(cx) {
            if let Some(path) = self.path_for_row(row, cx) {
                // Classify from the cached FileEntry, not a fresh stat.
                let is_dir = self
                    .active_tab()
                    .table
                    .read(cx)
                    .delegate()
                    .entries
                    .get(row)
                    .map(|e| matches!(e.kind, ferail_core::EntryKind::Directory))
                    .unwrap_or(false);
                let kind = if already_favorite(&path, self) || is_dir {
                    FavoriteResolved::Folder
                } else {
                    FavoriteResolved::NotAFolder
                };
                return Some((path, kind));
            }
        }
        let current = self.active_tab().current_dir.clone();
        Some((current, FavoriteResolved::Folder))
    }

    // ----- Tab management (5.5.d) ---------------------------------

    /// Cmd+T: open a new tab beside the active one, at the active
    /// tab's current directory. Spec §4.3: "new tab default — same
    /// directory as the currently active tab (so Cmd+T is 'another
    /// view of where I am'), inserted after the active tab."
    fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.active_tab().current_dir.clone();
        let id = self.process.fs.id_for_path(&path);
        self.process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(path.clone(), id);
        let tab = self.make_tab(path.clone(), id, window, cx);
        let insert_at = self.active + 1;
        self.tabs.insert(insert_at, tab);
        self.active = insert_at;
        self.load_path(path, cx);
    }

    /// Cmd+W: close the active tab. Per spec §3.4: if it's the last
    /// tab, close the whole window. With multi-window (Phase C) the
    /// process stays resident at zero windows, so this is non-fatal.
    /// Phase D: every closed tab pushes a snapshot onto
    /// `ProcessState::closed_tabs` for `Cmd+Shift+T`. Closing the
    /// last tab via this path pushes that final tab before the
    /// window is removed.
    /// Stop a tab's in-flight background work before it goes away.
    /// Trips the cooperative cancel flags (the worker holds its own
    /// `Arc` clone, so merely dropping the tab would let it keep walking
    /// / hashing the disk) and ends its task-registry row so the status
    /// bar doesn't show a ghost "Searching…" / "Finding duplicates…".
    /// Covers directory enumeration, folder-size, search, and duplicate
    /// scans — they all ride `load_cancel` / `load_task`.
    fn dismiss_tab_work(&self, tab: &Tab) {
        if let Some(cancel) = &tab.load_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = &tab.folder_size_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = &tab.prefetch_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(id) = tab.load_task {
            self.process.tasks.borrow_mut().end(id);
        }
    }

    fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            self.dismiss_tab_work(&self.tabs[self.active]);
            self.process
                .push_closed_tab(self.tabs[self.active].snapshot_for_close());
            window.remove_window();
            return;
        }
        self.dismiss_tab_work(&self.tabs[self.active]);
        let snapshot = self.tabs[self.active].snapshot_for_close();
        self.process.push_closed_tab(snapshot);
        self.tabs.remove(self.active);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        // The closed tab may have been the last reference to its
        // directory's file watch.
        let own_dirs: Vec<PathBuf> = self.tabs.iter().map(|t| t.current_dir.clone()).collect();
        self.process.prune_watches(cx.entity_id(), own_dirs, cx);
        // No re-load — the now-active tab already has its own
        // TableState populated from its earlier load (Phase A+B).
        cx.notify();
    }

    /// Cmd+Shift+W: close the entire window regardless of how many
    /// tabs it has. Per spec §3.4 the "I mean the window" verb. The
    /// process stays resident at zero windows; the user can reopen
    /// with Cmd+N. Phase D: all tabs are pushed onto the closed-tab
    /// stack in left-to-right order so the most-recent `Cmd+Shift+T`
    /// brings back the rightmost tab first (chronological reverse of
    /// individual closes).
    fn on_close_window(&mut self, _: &CloseWindow, window: &mut Window, _cx: &mut Context<Self>) {
        for tab in &self.tabs {
            self.dismiss_tab_work(tab);
            self.process.push_closed_tab(tab.snapshot_for_close());
        }
        window.remove_window();
    }

    /// Cmd+Shift+T: reopen the most recently closed tab. Pops the top
    /// of `ProcessState::closed_tabs`, builds a fresh tab at the
    /// recorded directory, restores filter/history/selection, and
    /// schedules a streaming reload. Spec §3.3 "Reopen closed tab".
    fn on_reopen_closed_tab(
        &mut self,
        _: &ReopenClosedTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(closed) = self.process.pop_closed_tab() else {
            return;
        };
        let path = closed.current_dir.clone();
        // Re-register the path in NodeStore to mint (or reuse) a
        // stable NodeId. ProcessState is the singleton, so a NodeId
        // captured before the tab closed is still valid — but we
        // pass through `get_or_create_path_with_id` regardless, the
        // same way Cmd+T does, so the reopen path stays a normal
        // "new tab at this path" pipeline.
        let node_id = self.process.fs.id_for_path(&path);
        self.process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(path.clone(), node_id);

        let mut tab = self.make_tab(path.clone(), node_id, window, cx);
        // Apply the captured tab-local state onto the fresh Tab
        // before inserting it. Filter goes onto both the Tab field
        // (which the load reads) and the live `Input` entity so the
        // title-bar field renders the restored text immediately.
        tab.history = closed.history;
        tab.history_index = closed.history_index;
        tab.filter_text = closed.filter_text.clone();
        tab.selection = closed.selection;
        tab.selection_all = false;
        tab.anchor = closed.anchor;
        tab.lead = closed.lead;
        let filter_input = tab.filter_input.clone();
        filter_input.update(cx, |state, cx| {
            state.set_value(closed.filter_text, window, cx);
        });

        let insert_at = self.active + 1;
        self.tabs.insert(insert_at, tab);
        self.active = insert_at;
        // Stream the directory fresh. The captured `selection` set
        // is reconciled against the model on streaming `Done` via
        // the standard reconciliation path — best-effort per spec
        // §3.3 (rows that no longer exist drop, surviving rows
        // re-light).
        self.load_path(path, cx);
    }

    /// Ctrl+Tab: cycle to the next tab.
    fn on_next_tab(&mut self, _: &NextTab, _: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 {
            return;
        }
        self.active = (self.active + 1) % self.tabs.len();
        cx.notify();
    }

    /// Ctrl+Shift+Tab: cycle to the previous tab.
    fn on_prev_tab(&mut self, _: &PrevTab, _: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 {
            return;
        }
        self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        cx.notify();
    }

    /// Switch to the tab at `idx`. Used by tabstrip click handlers.
    /// No re-enumeration — the target tab already owns its own
    /// `TableState` with whatever its last load produced.
    pub fn select_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() || idx == self.active {
            return;
        }
        self.active = idx;
        cx.notify();
    }

    /// Phase D, spec §3.3 "Reorder tab" — move the tab identified by
    /// `from_id` into gap position `to_pos`. Gap positions number
    /// `0..=tabs.len()`: gap 0 is before the first tab, gap N is after
    /// the last. Drops at gap-of-itself or gap-just-after-itself are
    /// no-ops (dropping where you started). Active-tab tracking is by
    /// `TabId`, so the active tab follows its own move and unrelated
    /// reorders correctly shift `self.active`.
    pub fn reorder_tab(&mut self, from_id: TabId, to_pos: usize, cx: &mut Context<Self>) {
        let Some(from_idx) = self.tabs.iter().position(|t| t.id == from_id) else {
            return;
        };
        // Index math (incl. the no-op and out-of-range rules) lives
        // in the pure, unit-tested `tab::reorder_insert_index`.
        let Some(insert_at) = tab::reorder_insert_index(from_idx, to_pos, self.tabs.len()) else {
            return;
        };
        let active_id = self.tabs[self.active].id;
        let tab = self.tabs.remove(from_idx);
        self.tabs.insert(insert_at, tab);
        self.active = self
            .tabs
            .iter()
            .position(|t| t.id == active_id)
            .unwrap_or(0);
        cx.notify();
    }

    fn on_navigate_parent(&mut self, _: &NavigateParent, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate_parent(cx);
    }

    /// User activated a row (double-click or Enter). For directories
    /// we navigate into them; for files we hand off to the OS opener.
    pub fn activate_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        // Archive rows are virtual: they have no on-disk path, and the
        // fallback below would otherwise synthesize `current_dir/<name>` and
        // try to open a file that doesn't exist. Activating a folder opens it
        // in the tree instead; files do nothing until in-archive preview lands.
        {
            let table = self.active_tab().table.clone();
            let toggle = {
                let t = table.read(cx);
                let del = t.delegate();
                del.is_archive_mode()
                    .then(|| {
                        del.archive_path_for_row(row_ix)
                            .map(str::to_string)
                            .zip(del.archive_view.clone())
                    })
                    .flatten()
            };
            if let Some((path, view)) = toggle {
                let _ = view.update(cx, |v, cx| v.toggle_expanded(&path, cx));
                return;
            }
            if table.read(cx).delegate().is_archive_mode() {
                return;
            }
        }
        let path_and_kind = self
            .active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .get(row_ix)
            .map(|e| {
                (
                    self.path_for_row(row_ix, cx).unwrap_or_else(|| {
                        let mut p = self.active_tab().current_dir.clone();
                        p.push(e.name.as_ref());
                        p
                    }),
                    e.kind,
                )
            });
        let Some((path, kind)) = path_and_kind else {
            return;
        };
        match kind {
            EntryKind::Directory => self.navigate(path, cx),
            EntryKind::Symlink if crate::platform_shell::is_wsl_path(&path) => {
                self.activate_wsl_symlink(path, cx);
            }
            EntryKind::File | EntryKind::Symlink => {
                // Platform Shell owns default invocation: `open(1)` on macOS,
                // ShellExecuteExW on Windows, and xdg-open on Linux. All can
                // cross slow association/provider boundaries, so even this
                // single-file action stays off the UI thread.
                cx.background_spawn(async move {
                    let _ = crate::platform_shell::open_with_default(&path);
                })
                .detach();
            }
        }
    }

    /// Resolve an explicitly activated WSL symlink once, off-thread. Windows
    /// exposes links such as `/bin -> /usr/bin` as rows but cannot always
    /// follow them through UNC APIs. A resolved directory re-enters normal
    /// navigation; a resolved file goes through the normal default opener.
    fn activate_wsl_symlink(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let tab_id = self.active_tab().id;
        let generation = self.active_tab().load_generation;
        if let Some(cancel) = self.active_tab_mut().wsl_resolve_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.active_tab_mut().wsl_resolve_cancel = Some(cancel.clone());
        cx.spawn(async move |this, cx| {
            let worker_path = path.clone();
            let worker_cancel = cancel.clone();
            let resolved = cx
                .background_executor()
                .spawn(async move {
                    let resolved = crate::platform_shell::resolve_wsl_symlink_path(
                        &worker_path,
                        &worker_cancel,
                    )?;
                    let is_dir = wsl_resolved_target_is_dir(&resolved);
                    Ok::<_, ferail_core::platform_locations::PlatformRootErrorKind>((
                        resolved, is_dir,
                    ))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(tab_index) = this.tabs.iter().position(|tab| tab.id == tab_id) else {
                    return;
                };
                if !this.tabs[tab_index]
                    .wsl_resolve_cancel
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &cancel))
                {
                    return;
                }
                this.tabs[tab_index].wsl_resolve_cancel = None;
                if tab_index != this.active
                    || this.tabs[tab_index].load_generation != generation
                    || cancel.load(Ordering::Relaxed)
                {
                    return;
                }
                match resolved {
                    Ok((target, true)) => this.navigate(target, cx),
                    Ok((target, false)) => {
                        cx.background_spawn(async move {
                            let _ = crate::platform_shell::open_with_default(&target);
                        })
                        .detach();
                    }
                    Err(_) => {
                        // Preserve the pre-WSL fallback for an unresolved or
                        // broken link: let the OS try its normal association.
                        cx.background_spawn(async move {
                            let _ = crate::platform_shell::open_with_default(&path);
                        })
                        .detach();
                    }
                }
            });
        })
        .detach();
    }

    /// Navigate to the parent of the current directory (Backspace
    /// keybind in 4.c.2). No-op when already at the filesystem root.
    pub fn navigate_parent(&mut self, cx: &mut Context<Self>) {
        let cur = self.active_tab().current_dir.clone();
        if let Some(parent) = cur.parent() {
            let parent = parent.to_path_buf();
            if parent != cur {
                self.navigate(parent, cx);
                // Select the folder we came from so it's highlighted and
                // scrolled into view once the parent's contents stream
                // in — `cur` is by definition an immediate child of
                // `parent`. `navigate` just cleared selection; seed it
                // here and the streaming reconcile
                // (`refresh_file_list_selection_in_tab`) applies + reveals
                // it, matching what Back does via `history_child_to_select`.
                let child_id = self.process.fs.id_for_path(&cur);
                let tab = self.active_tab_mut();
                tab.selection.insert(child_id);
                tab.anchor = Some(child_id);
                tab.lead = Some(child_id);
            }
        }
    }

    /// Navigate to a path that entered the app from OUTSIDE (typed
    /// breadcrumb input today; any future "go to folder" prompt).
    /// The path-identity contract says external paths canonicalize
    /// once at their entry boundary — but canonicalize is a stat
    /// call, so it runs on the background executor, then the real
    /// `navigate` applies on the main thread. A failed canonicalize
    /// falls back to the typed path; navigation's enumeration owns
    /// the error reporting.
    pub fn navigate_external(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let canonical = cx
                .background_executor()
                .spawn(async move { path::canonicalize_for_identity(path) })
                .await;
            let _ = this.update(cx, |this, cx| this.navigate(canonical, cx));
        })
        .detach();
    }

    /// Navigate to `path`: snapshot the current tab's selection
    /// into the history entry we're leaving, re-enumerate, refresh
    /// the Table, push to history (truncating any forward stack
    /// first), reset selection for the new path, and increment the
    /// Ant Trail visit count (Stage 9.b).
    ///
    /// Spec §2.6: navigation commits immediately and starts the
    /// new path with empty selection unless this is a back/forward
    /// (see `navigate_back` / `navigate_forward` which seed the
    /// restored selection before calling here).
    pub fn navigate(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.navigate_with_tracking(path, true, cx);
    }

    /// Open `dir` in a **new tab** and select `names` there once the rows
    /// arrive.
    ///
    /// Backs the extract confirmation: extraction writes into the archive's
    /// folder, which is often not the folder on screen, so the toast is
    /// clickable and lands the user on the result with it selected.
    ///
    /// A new tab rather than navigating in place, for two reasons. It never
    /// costs the user the folder they were in — extraction is a side errand,
    /// not a navigation. And it always *visibly* does something: when the
    /// destination happens to be the folder already on screen, navigating
    /// would do nothing at all and only select a row, which the first
    /// version of this did — off-screen in a long listing, that reads as a
    /// dead link.
    ///
    /// The selection is queued by name because the new tab has not
    /// enumerated yet (see [`Tab::pending_select_names`]).
    pub fn reveal_in_new_tab(
        &mut self,
        dir: PathBuf,
        names: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_path_in_new_tab(dir, window, cx);
        if !names.is_empty() {
            self.active_tab_mut().pending_select_names = names;
        }
        cx.notify();
    }

    /// Navigate originating from a favorite (sidebar click / Enter). When
    /// the "exclude favorites from tracking" setting is on — the default
    /// — the visit is *not* recorded: clicking a favorite is a deliberate
    /// shortcut, not organic browsing, so it shouldn't inflate the
    /// folder's Ant Trail heat or push it into Recents. The same folder
    /// reached by browsing still records normally.
    pub fn navigate_from_favorite(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let record = !crate::ant_trail::exclude_favorites(cx);
        self.navigate_with_tracking(path, record, cx);
    }

    /// Warm the child-folder list for a breadcrumb segment off the UI
    /// thread (Prime Directive: no `read_dir` on the menu/render path).
    /// Feeds the segment's "Go to Subfolder" submenu. No-op once an
    /// entry exists (in-flight `None` or completed `Some`).
    pub fn warm_breadcrumb_children(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.breadcrumb_children.contains_key(&path) {
            return;
        }
        // Mark in-flight so repeated menu opens don't respawn.
        self.breadcrumb_children.insert(path.clone(), None);
        let dir = path.clone();
        let task = cx.background_spawn(async move {
            let mut out: Vec<(SharedString, PathBuf)> = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let Some(name) = e.file_name().to_str().map(|s| s.to_string()) else {
                        continue;
                    };
                    if name.starts_with('.') {
                        continue;
                    }
                    // `is_dir` follows symlinks so a link-to-folder is a
                    // valid navigation target, matching path completion.
                    if !e.path().is_dir() {
                        continue;
                    }
                    let display = ferail_fs_native::paths::display_leaf(&name).into_owned();
                    out.push((SharedString::from(display), e.path()));
                }
            }
            out.sort_by_key(|a| a.0.to_lowercase());
            out.truncate(100);
            out
        });
        cx.spawn(async move |this, cx| {
            let children = task.await;
            let _ = this.update(cx, |this, cx| {
                this.breadcrumb_children
                    .insert(path, Some(Rc::new(children)));
                cx.notify();
            });
        })
        .detach();
    }

    /// Clicking a Tag favorite (§9): run a Finder-tag search in the
    /// active tab, replacing its listing with the tagged items. Uses the
    /// same streaming search surface as text search.
    pub fn navigate_from_tag_favorite(
        &mut self,
        tag: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab_id = self.active_tab().id;
        let notify = Some(window.window_handle());
        self.start_tag_search(tab_id, tag, notify, cx);
    }

    /// Shared navigation body. `record_visit` gates the Ant Trail +
    /// Recents bump (see [`Shell::navigate_from_favorite`]); the public
    /// [`Shell::navigate`] always records.
    fn navigate_with_tracking(
        &mut self,
        path: PathBuf,
        record_visit: bool,
        cx: &mut Context<Self>,
    ) {
        crate::log_info!(90, "navigate: {}", path.display());
        crate::trail::navigate(crate::trail::NavKind::Go, &path);
        let node_id = self.process.fs.id_for_path(&path);
        self.process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(path.clone(), node_id);
        let tab = self.active_tab_mut();
        // Snapshot the selection we're leaving into the current
        // history entry so a Back returns to where the user was.
        if let Some(entry) = tab.history.get_mut(tab.history_index) {
            entry.selection = if tab.selection_all {
                HashSet::new()
            } else {
                tab.selection.clone()
            };
            entry.anchor = tab.anchor;
            entry.lead = tab.lead;
        }
        let same_path = tab
            .history
            .get(tab.history_index)
            .map(|e| e.path == path)
            .unwrap_or(false);
        if !same_path {
            tab.history.truncate(tab.history_index + 1);
            tab.history.push(HistoryEntry::new(path.clone()));
            tab.history_index = tab.history.len() - 1;
        }
        // Fresh navigation: clear selection + filter holding +
        // live-range. Back/forward override this with the restored
        // snapshot just before calling us (see `restore_from_history`).
        tab.clear_selection();
        tab.anchor = None;
        tab.lead = None;
        tab.filtered_out.clear();
        tab.range_live = false;
        // Leaving any results view: this commits a real directory.
        tab.tool_result = None;
        tab.nav.navigate_to(node_id);
        // Any pending screenshot select belongs to the previous
        // path; drop it so a stale row index doesn't apply. Same for
        // queued post-op names — a leftover name must not select a
        // same-named entry in an unrelated folder.
        self.active_tab_mut().pending_select_row = None;
        self.active_tab_mut().pending_select_rows.clear();
        self.active_tab_mut().pending_select_names.clear();
        if record_visit {
            self.record_ant_visit(node_id, cx);
        }
        self.load_path(path, cx);
    }

    pub fn navigate_node(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
        let Some(path) = self
            .process
            .node_store
            .borrow_mut()
            .path_snapshot_for_job(node_id, "Shell::navigate_node")
        else {
            return;
        };
        self.navigate(path, cx);
    }

    /// Bump the Ant Trail visit count for `path` in the in-memory
    /// map and persist asynchronously through `metadata_db`. Cheap
    /// on the foreground executor — the DB write is a single upsert
    /// dispatched to the background executor (one shared connection
    /// behind a mutex; there is no pooling).
    pub fn record_ant_visit(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
        let Some(path) = self
            .process
            .node_store
            .borrow()
            .path_snapshot_for_job(node_id, "Shell::record_ant_visit")
        else {
            return;
        };
        self.process.record_ant_visit(path.clone());
        // Recents is a separate, user-toggleable view over the same
        // visit log: only feed it when the feature is on. The DB write
        // below still runs so the Ant Trail (its own switch) keeps heat.
        if crate::recents_section::recents_enabled(cx) {
            self.process.push_recent(path.clone());
        }
        let heat = self.ant_heat(node_id);
        self.process.node_store.borrow_mut().set_heat(node_id, heat);
        if let Some(db) = self.process.db_snapshot() {
            let path_str = path.to_string_lossy().into_owned();
            let when = now_unix_secs();
            cx.background_executor()
                .spawn(async move {
                    if let Ok(guard) = db.lock() {
                        let _ = guard.record_folder_visit(&path_str, when);
                    }
                })
                .detach();
        }
    }

    /// Compute the Ant Trail heat for `path` — 0.0 (never visited)
    /// through 1.0 (most-visited folder). Log-scaled so a 10-visit
    /// folder isn't 10× brighter than a 5-visit one. Used by the
    /// file list to apply a subtle background tint per row.
    pub fn ant_heat(&self, node_id: NodeId) -> f32 {
        let cached = self.process.node_store.borrow().heat(node_id);
        if cached > 0.0 {
            return cached;
        }
        let Some(path) = self
            .process
            .node_store
            .borrow()
            .path_snapshot_for_job(node_id, "Shell::ant_heat")
        else {
            return 0.0;
        };
        let Some(&v) = self.process.ant_visits.borrow().get(&path) else {
            return 0.0;
        };
        let max = self.process.ant_max.get();
        if max <= 1 {
            return 1.0;
        }
        ((v as f32 + 1.0).log2() / (max as f32 + 1.0).log2()).clamp(0.0, 1.0)
    }

    /// Toggle expansion for a directory in the sidebar tree.
    /// Collapsing also removes every descendant from `expanded` so a
    /// future re-open doesn't carry stale sub-expansions forward.
    /// Cache stays — re-expand is instantaneous.
    pub fn toggle_tree_expand(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.expanded.contains(path) {
            let prefix = path.to_path_buf();
            self.expanded.retain(|p| !p.starts_with(&prefix));
        } else {
            self.expanded.insert(path.to_path_buf());
            if !self.tree_children.contains_key(path) {
                self.start_tree_children_load(path.to_path_buf(), cx);
            }
        }
        cx.notify();
    }

    /// Spring-load a sidebar tree row: while a drag dwells over a
    /// collapsed folder, expand it (open-only, never collapse) so the
    /// user can drill the tree without releasing. Called from the tree
    /// row's `on_drag_move` (docs/features/FILE_OPS.md).
    pub fn tree_drag_hover(&mut self, path: &Path, cx: &mut Context<Self>) {
        const DWELL: std::time::Duration = std::time::Duration::from_millis(600);
        let now = std::time::Instant::now();
        match &self.tree_spring {
            Some((p, since)) if p == path => {
                if now.duration_since(*since) >= DWELL {
                    self.tree_spring = None;
                    if !self.expanded.contains(path) {
                        self.toggle_tree_expand(path, cx);
                    }
                }
            }
            _ => self.tree_spring = Some((path.to_path_buf(), now)),
        }
    }

    pub fn toggle_tree_expand_node(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
        let Some(path) = self
            .process
            .node_store
            .borrow_mut()
            .path_snapshot_for_job(node_id, "Shell::toggle_tree_expand_node")
        else {
            return;
        };
        self.toggle_tree_expand(&path, cx);
    }

    fn start_tree_children_load(&self, path: PathBuf, cx: &mut Context<Self>) {
        let fs = self.process.fs.clone();
        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            let parent = path.clone();
            let children = cx
                .background_executor()
                .spawn(async move { run_tree_children_load(fs, parent.clone()) })
                .await;
            let Some(shell) = weak.upgrade() else { return };
            shell.update(cx, |this, cx| {
                for child in &children {
                    this.process
                        .node_store
                        .borrow_mut()
                        .get_or_create_path_with_id(child.path.clone(), child.node_id);
                }
                this.tree_children.insert(path, children);
                cx.notify();
            });
        })
        .detach();
    }

    /// Make sure `tree_children[path]` is populated. Cheap no-op if
    /// already cached. On first call, runs `std::fs::read_dir`
    /// synchronously (consistent with `Shell::load_path`'s sync
    /// enumeration; a unified async-streaming refactor lives in a
    /// later iter). Folder-only — files don't appear in the tree.
    ///
    /// Hidden entries are *included* in the cache; the renderer
    /// filters them out based on the live `show_hidden` flag so a
    /// toggle doesn't require cache invalidation.
    pub fn ensure_tree_children(&mut self, path: &Path) {
        if self.tree_children.contains_key(path) {
            return;
        }
        let mut children: Vec<TreeChild> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(path) {
            for dirent in rd.flatten() {
                let p = dirent.path();
                let Some(name) = p.file_name().and_then(|s| s.to_str()).map(str::to_owned) else {
                    continue;
                };
                // file_type() can be cheap (no extra stat on most
                // platforms); fall back to metadata() if it errors.
                let is_dir = match dirent.file_type() {
                    Ok(ft) => {
                        ft.is_dir()
                            || (ft.is_symlink()
                                && std::fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false))
                    }
                    Err(_) => false,
                };
                if !is_dir {
                    continue;
                }
                // Same platform hidden contract as run_tree_children_load.
                let hidden = std::fs::symlink_metadata(&p)
                    .map(|m| ferail_fs_native::entry_is_hidden(&name, &m))
                    .unwrap_or_else(|_| name.starts_with('.'));
                let node_id = self.process.fs.id_for_path(&p);
                self.process
                    .node_store
                    .borrow_mut()
                    .get_or_create_path_with_id(p.clone(), node_id);
                let has_subdirs = dir_has_subdir(&p);
                // Tree label is the displayed leaf (macOS `:` → `/`); the raw
                // `name` above already drove the hidden check and `path` drives
                // navigation, so the swap is display-only.
                let label = ferail_fs_native::paths::display_leaf(&name).into_owned();
                children.push(TreeChild {
                    node_id,
                    path: p,
                    label,
                    hidden,
                    has_subdirs,
                });
            }
            children.sort_by_key(|a| a.label.to_lowercase());
        }
        self.tree_children.insert(path.to_path_buf(), children);
    }

    /// Expand `path` and every ancestor in the tree. Used by the
    /// `--expand <path>` CLI flag to reveal a path with the
    /// surrounding hierarchy unfurled. Each directory's children
    /// are also enumerated into
    /// `tree_children` so the first frame already has them.
    pub fn reveal_path_in_tree(&mut self, path: &Path) {
        let mut chain: Vec<PathBuf> = vec![path.to_path_buf()];
        let mut cur = path.parent().map(|p| p.to_path_buf());
        while let Some(a) = cur {
            chain.push(a.clone());
            cur = a.parent().map(|p| p.to_path_buf());
        }
        // Walk from filesystem root toward `path` so each
        // enumeration sees its parent already populated (no
        // correctness impact, but symmetric with how the tree
        // builds top-down).
        for a in chain.into_iter().rev() {
            self.expanded.insert(a.clone());
            self.ensure_tree_children(&a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        UndoOp, collapse_error_summary, copy_back_moved_item, file_op_failure_report,
        history_child_to_select, window_title_for,
    };
    use std::path::{Path, PathBuf};

    /// Fresh per-test scratch dir (same pattern as ferail-fs-native's
    /// file-op tests).
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ferail-shell-undo-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(p: &Path, contents: &[u8]) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, contents).unwrap();
    }

    // ---- MoveBackCross (cross-volume move undo) apply semantics ----
    //
    // `copy_back_moved_item` is volume-agnostic (engine copy + delete),
    // so a same-volume tempdir exercises the exact code path the
    // cross-volume undo runs.

    #[test]
    fn move_back_cross_restores_tree_and_removes_moved_copy() {
        let root = scratch("moveback-tree");
        // Forward move happened earlier: home/proj → elsewhere/proj.
        let original = root.join("home/proj");
        std::fs::create_dir_all(root.join("home")).unwrap();
        let moved = root.join("elsewhere/proj");
        write(&moved.join("a.txt"), b"hello");
        write(&moved.join("sub/b.bin"), &[7u8; 512]);

        copy_back_moved_item(&original, &moved).unwrap();

        assert_eq!(std::fs::read(original.join("a.txt")).unwrap(), b"hello");
        assert_eq!(
            std::fs::read(original.join("sub/b.bin")).unwrap().len(),
            512
        );
        assert!(
            !moved.exists(),
            "moved copy must be deleted after copy-back"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn move_back_cross_restores_keep_both_renamed_leaf() {
        let root = scratch("moveback-keepboth");
        // The forward move collided and Keep-Both landed as "a 2.txt";
        // undo must restore the *original* name, not "a 2.txt".
        let original = root.join("home/a.txt");
        std::fs::create_dir_all(root.join("home")).unwrap();
        let moved = root.join("elsewhere/a 2.txt");
        write(&moved, b"payload");

        copy_back_moved_item(&original, &moved).unwrap();

        assert_eq!(std::fs::read(&original).unwrap(), b"payload");
        assert!(!root.join("home/a 2.txt").exists());
        assert!(!moved.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn move_back_cross_refuses_reoccupied_original_and_keeps_moved() {
        let root = scratch("moveback-occupied");
        let original = root.join("home/a.txt");
        write(&original, b"newcomer"); // something new appeared at the origin
        let moved = root.join("elsewhere/a.txt");
        write(&moved, b"payload");

        let err = copy_back_moved_item(&original, &moved).unwrap_err();
        assert!(err.contains("exists again"), "{err}");
        assert_eq!(
            std::fs::read(&original).unwrap(),
            b"newcomer",
            "the newcomer must not be clobbered"
        );
        assert_eq!(
            std::fs::read(&moved).unwrap(),
            b"payload",
            "the moved copy must survive a refused undo"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn move_back_cross_batch_continues_past_a_failed_pair() {
        let root = scratch("moveback-batch");
        let occupied_original = root.join("home/busy.txt");
        write(&occupied_original, b"newcomer");
        let occupied_moved = root.join("elsewhere/busy.txt");
        write(&occupied_moved, b"stuck");
        let free_original = root.join("home/free.txt");
        let free_moved = root.join("elsewhere/free.txt");
        write(&free_moved, b"comes home");

        let op = UndoOp::MoveBackCross(vec![
            (occupied_original.clone(), occupied_moved.clone()),
            (free_original.clone(), free_moved.clone()),
        ]);
        let err = op.apply_fs().unwrap_err();
        assert!(err.contains("exists again"), "{err}");
        // The failed pair kept both sides; the good pair still applied.
        assert_eq!(std::fs::read(&occupied_moved).unwrap(), b"stuck");
        assert_eq!(std::fs::read(&free_original).unwrap(), b"comes home");
        assert!(!free_moved.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- Shared partial-failure report ----

    #[test]
    fn failure_report_counts_lists_items_and_advises() {
        use ferail_fs_native::file_ops::{FileOpError, FileOpErrorKind};
        let failed = vec![
            FileOpError::other(
                Path::new("/x/Report.pdf"),
                FileOpErrorKind::PermissionDenied,
                "raw os text",
            ),
            FileOpError::other(Path::new("/x/notes.txt"), FileOpErrorKind::Locked, "raw"),
        ];
        let s = file_op_failure_report("Move to Trash", 3, 0, &failed);
        assert!(
            s.starts_with("Move to Trash: 3 of 5 done \u{00b7} 2 failed"),
            "{s}"
        );
        assert!(s.contains("Report.pdf \u{2014} permission denied"), "{s}");
        assert!(
            s.contains("notes.txt \u{2014} in use by another program"),
            "{s}"
        );
        // Permission denied dominates → elevation advice.
        assert!(s.contains("administrator"), "{s}");
    }

    #[test]
    fn failure_report_elides_a_long_tail_and_counts_skips() {
        use ferail_fs_native::file_ops::{FileOpError, FileOpErrorKind};
        let failed: Vec<FileOpError> = (0..6)
            .map(|i| {
                FileOpError::other(
                    Path::new("/x").join(format!("f{i}.txt")).as_path(),
                    FileOpErrorKind::Other,
                    "raw",
                )
            })
            .collect();
        let s = file_op_failure_report("Empty Trash", 2, 1, &failed);
        assert!(
            s.starts_with("Empty Trash: 2 of 9 done \u{00b7} 6 failed"),
            "{s}"
        );
        assert!(s.contains("\u{2026}and 2 more"), "{s}");
    }

    #[test]
    fn short_single_line_error_is_shown_whole_with_no_details_toggle() {
        let (summary, has_more) = collapse_error_summary("Move to Trash failed: nope");
        assert_eq!(summary, "Move to Trash failed: nope");
        assert!(
            !has_more,
            "a short, single-line error needs no Details toggle"
        );
    }

    #[test]
    fn long_error_is_truncated_and_offers_details() {
        let long = format!("Move to Trash failed: {}", "x".repeat(300));
        let (summary, has_more) = collapse_error_summary(&long);
        assert!(has_more, "a long error hides detail behind Details");
        assert!(
            summary.ends_with('\u{2026}'),
            "truncation is marked with an ellipsis"
        );
        assert!(
            summary.chars().count() <= 141,
            "headline stays one line: 140 chars + the ellipsis, got {}",
            summary.chars().count()
        );
    }

    #[test]
    fn multiline_error_collapses_to_its_first_line() {
        let (summary, has_more) = collapse_error_summary("Copy failed\n3 of 5 done\nretry?");
        assert_eq!(summary, "Copy failed\u{2026}");
        assert!(has_more, "later lines are hidden until expanded");
    }

    #[test]
    fn window_title_includes_folder_and_app_name() {
        assert_eq!(
            window_title_for(Path::new("/Users/jk/Documents")),
            "Documents \u{2014} Ferail"
        );
    }

    #[test]
    fn window_title_falls_back_to_full_path_at_a_root() {
        // A filesystem root has an empty `file_name()` on every
        // platform; show the path itself rather than dropping to the
        // bare app name. (`/` is a root with empty `file_name()` on
        // both Windows and Unix, so this stays platform-stable.)
        assert_eq!(window_title_for(Path::new("/")), "/ \u{2014} Ferail");
    }

    #[test]
    fn window_title_is_never_blank() {
        // Empty path → the switcher entry still reads the app name.
        assert_eq!(window_title_for(Path::new("")), "Ferail");
    }

    #[test]
    fn history_child_selects_direct_child_when_back_lands_on_parent() {
        assert_eq!(
            history_child_to_select(Path::new("/Users/jk/Projects"), Path::new("/Users/jk")),
            Some(PathBuf::from("/Users/jk/Projects"))
        );
    }

    #[test]
    fn history_child_selects_first_child_when_back_lands_on_ancestor() {
        assert_eq!(
            history_child_to_select(
                Path::new("/Users/jk/Projects/Ferail"),
                Path::new("/Users/jk")
            ),
            Some(PathBuf::from("/Users/jk/Projects"))
        );
    }

    #[test]
    fn history_child_ignores_same_or_unrelated_paths() {
        assert_eq!(
            history_child_to_select(Path::new("/Users/jk"), Path::new("/Users/jk")),
            None
        );
        assert_eq!(
            history_child_to_select(Path::new("/Users/jk/Projects"), Path::new("/tmp")),
            None
        );
    }
}
