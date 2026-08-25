use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use ferail_core::filter_expr::{DateCtx, FilterExpr};
use ferail_core::{EnumerationError, FileEntry, NodeId};
use ferail_fs_native::{DEFAULT_ENUMERATION_BATCH, NativeFs};
use gpui::SharedString;

use crate::tree::TreeChild;

pub(crate) struct LoadBatch {
    pub entries: Vec<FileEntry>,
    pub paths: HashMap<NodeId, PathBuf>,
}

/// Aggregate of the entries the hidden filter dropped from a load —
/// what the status bar shows ("N hidden · X B") when *show hidden* is
/// off, so hidden content is discoverable without unhiding it. Zero
/// when the toggle is on (nothing is skipped). `bytes` sums
/// `FileEntry::size`, so hidden *folders* count at their dirent size,
/// not their subtree total — same property as the status bar's item
/// total.
// `pub` (not `pub(crate)`) only to satisfy private-interfaces on the
// `Tab::hidden_summary` field; the private `loading` module bounds the
// real reach to the shell tree.
#[derive(Clone, Copy, Default)]
pub struct HiddenSummary {
    pub count: usize,
    pub bytes: u64,
}

/// Aggregate of the entries the *filter field* dropped from a load —
/// what the status bar shows ("N filtered out · X B") while a filter is
/// typed, so the count and size stay honest about the whole folder
/// instead of only the matches. Zero when the field is empty.
/// Counted *after* the hidden partition, so a hidden entry that also
/// fails the needle is reported once, as hidden.
///
/// Same shape as [`HiddenSummary`] but a distinct type, so the two
/// skipped-entry totals can never be swapped at a call site.
#[derive(Clone, Copy, Default)]
pub struct FilterSummary {
    pub count: usize,
    pub bytes: u64,
}

pub(super) enum LoadMsg {
    Batch(LoadBatch),
    /// End of stream: the enumeration error (if any) and the totals of
    /// entries skipped across the whole load — hidden ones, then the
    /// ones the filter field excluded. Carried on `Done` rather than
    /// per-batch because `Done` is sent exactly once and never dropped
    /// (empty batches are), and the status bar only needs the final
    /// figures.
    Done(Option<EnumerationError>, HiddenSummary, FilterSummary),
}

/// Clock + local-zone context for resolving the filter's date tokens
/// (`mod:today`, `created:>2026-01-01`). Built fresh per load so "today"
/// stays honest across midnight; called on workers, never in paint.
pub(super) fn filter_date_ctx() -> DateCtx {
    DateCtx {
        now_unix: ferail_core::now_unix(),
        tz_offset_secs: ferail_fs_native::stat_info::local_tz_offset_secs(),
    }
}

pub(super) fn run_directory_load_streaming(
    fs: Arc<NativeFs>,
    path: PathBuf,
    show_hidden: bool,
    filter_text: String,
    cancel: Arc<AtomicBool>,
    tx: async_channel::Sender<LoadMsg>,
) {
    let expr = FilterExpr::parse(filter_text.trim(), filter_date_ctx());
    let mut hidden = HiddenSummary::default();
    let mut filtered = FilterSummary::default();
    let error = fs.enumerate_streaming(&path, DEFAULT_ENUMERATION_BATCH, &cancel, |entries| {
        let (batch, skipped, excluded) = filter_directory_batch(&fs, entries, show_hidden, &expr);
        hidden.count += skipped.count;
        hidden.bytes += skipped.bytes;
        filtered.count += excluded.count;
        filtered.bytes += excluded.bytes;
        if !batch.entries.is_empty() && tx.send_blocking(LoadMsg::Batch(batch)).is_err() {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    let _ = tx.send_blocking(LoadMsg::Done(error, hidden, filtered));
}

fn filter_directory_batch(
    fs: &NativeFs,
    entries: Vec<FileEntry>,
    show_hidden: bool,
    expr: &FilterExpr,
) -> (LoadBatch, HiddenSummary, FilterSummary) {
    // Hidden partition FIRST, text filter second: the hidden summary
    // must not change while the user types a filter, so it counts every
    // hidden entry the listing dropped, needle or no needle.
    // `hidden` carries platform semantics (BSD UF_HIDDEN on macOS,
    // FILE_ATTRIBUTE_HIDDEN on Windows) resolved at enumerate time —
    // never re-derive from the name here.
    let (visible, skipped): (Vec<FileEntry>, Vec<FileEntry>) =
        entries.into_iter().partition(|e| show_hidden || !e.hidden);
    let hidden = HiddenSummary {
        count: skipped.len(),
        bytes: skipped.iter().map(|e| e.size).sum(),
    };
    // `matches_entry` searches the visible Format value too —
    // otherwise typing "pdf document" or "zip archive" misses rows
    // where the magic-detected text is the only place those phrases
    // appear. Structured tokens (size:, mod:, locked:, …) test the
    // row's cached metadata fields — never fresh I/O.
    let (entries, excluded): (Vec<FileEntry>, Vec<FileEntry>) =
        visible.into_iter().partition(|e| expr.matches_entry(e));
    let filtered = FilterSummary {
        count: excluded.len(),
        bytes: excluded.iter().map(|e| e.size).sum(),
    };
    let mut paths = HashMap::with_capacity(entries.len());
    for entry in &entries {
        if let Some(path) = fs.path_for(entry.id) {
            paths.insert(entry.id, path);
        }
    }
    (LoadBatch { entries, paths }, hidden, filtered)
}

#[cfg(test)]
mod filter_tests {
    use super::*;
    use ferail_core::EntryKind;

    fn entry(name: &str, size: u64, hidden: bool) -> FileEntry {
        FileEntry {
            id: NodeId::from(size + u64::from(hidden)),
            name: name.into(),
            display_name: name.into(),
            name_has_hazards: false,
            kind: EntryKind::File,
            size,
            mtime_unix: 0,
            display_size: ferail_core::empty_entry_text(),
            display_kind: "Document".into(),
            display_magic: ferail_core::empty_entry_text(),
            display_description: ferail_core::empty_entry_text(),
            details_loaded: false,
            is_quarantined: false,
            quarantine: None,
            hidden,
            created_unix: None,
            locked: false,
        }
    }

    fn batch(
        entries: Vec<FileEntry>,
        show_hidden: bool,
        needle: &str,
    ) -> (Vec<String>, HiddenSummary) {
        let (names, hidden, _) = batch_full(entries, show_hidden, needle);
        (names, hidden)
    }

    fn batch_full(
        entries: Vec<FileEntry>,
        show_hidden: bool,
        needle: &str,
    ) -> (Vec<String>, HiddenSummary, FilterSummary) {
        let fs = NativeFs::new();
        let expr = FilterExpr::parse(needle, filter_date_ctx());
        let (batch, hidden, filtered) = filter_directory_batch(&fs, entries, show_hidden, &expr);
        (
            batch
                .entries
                .into_iter()
                .map(|e| e.name.to_string())
                .collect(),
            hidden,
            filtered,
        )
    }

    #[test]
    fn hidden_off_counts_and_sums_skipped() {
        let (names, hidden) = batch(
            vec![
                entry("a.txt", 10, false),
                entry(".env", 5, true),
                entry(".git", 300, true),
            ],
            false,
            "",
        );
        assert_eq!(names, vec!["a.txt"]);
        assert_eq!(hidden.count, 2);
        assert_eq!(hidden.bytes, 305);
    }

    #[test]
    fn hidden_on_reports_zero() {
        let (names, hidden) = batch(
            vec![entry("a.txt", 10, false), entry(".env", 5, true)],
            true,
            "",
        );
        assert_eq!(names, vec!["a.txt", ".env"]);
        assert_eq!(hidden.count, 0);
        assert_eq!(hidden.bytes, 0);
    }

    #[test]
    fn text_filter_does_not_perturb_hidden_summary() {
        // The needle drops every visible row; the hidden aggregate must
        // still report the full skipped set, not shrink with the filter.
        let (names, hidden) = batch(
            vec![entry("a.txt", 10, false), entry(".env", 5, true)],
            false,
            "no-match-zzz",
        );
        assert!(names.is_empty());
        assert_eq!(hidden.count, 1);
        assert_eq!(hidden.bytes, 5);
    }

    #[test]
    fn all_hidden_batch_still_reports() {
        // The worker drops empty batches on the channel; the summary
        // must survive independently (it rides `Done`, not `Batch`).
        let (names, hidden) = batch(vec![entry(".a", 1, true), entry(".b", 2, true)], false, "");
        assert!(names.is_empty());
        assert_eq!(hidden.count, 2);
        assert_eq!(hidden.bytes, 3);
    }

    #[test]
    fn filter_summary_counts_and_sums_non_matches() {
        let (names, _, filtered) = batch_full(
            vec![
                entry("report.pdf", 10, false),
                entry("notes.txt", 200, false),
                entry("photo.jpg", 3000, false),
            ],
            false,
            "report",
        );
        assert_eq!(names, vec!["report.pdf"]);
        assert_eq!(filtered.count, 2);
        assert_eq!(filtered.bytes, 3200);
    }

    #[test]
    fn empty_needle_filters_nothing() {
        let (names, _, filtered) = batch_full(
            vec![entry("a.txt", 10, false), entry("b.txt", 5, false)],
            false,
            "",
        );
        assert_eq!(names.len(), 2);
        assert_eq!(filtered.count, 0);
        assert_eq!(filtered.bytes, 0);
    }

    #[test]
    fn value_tokens_filter_on_metadata() {
        // `size:` reads the cached size field, no name involvement; the
        // non-matching row still lands in the filtered-out summary.
        let (names, _, filtered) = batch_full(
            vec![entry("big.bin", 5000, false), entry("small.txt", 10, false)],
            false,
            "size:>1kb",
        );
        assert_eq!(names, vec!["big.bin"]);
        assert_eq!(filtered.count, 1);
        assert_eq!(filtered.bytes, 10);
        // Tokens AND with plain text.
        let (names, _, _) = batch_full(
            vec![
                entry("big.bin", 5000, false),
                entry("huge.txt", 9000, false),
            ],
            false,
            "size:>1kb huge",
        );
        assert_eq!(names, vec!["huge.txt"]);
    }

    #[test]
    fn hidden_entries_are_not_counted_twice() {
        // `.env` fails the needle too, but it was already dropped as
        // hidden — the filter aggregate must not claim it as well.
        let (names, hidden, filtered) = batch_full(
            vec![entry("a.txt", 10, false), entry(".env", 5, true)],
            false,
            "zzz",
        );
        assert!(names.is_empty());
        assert_eq!(hidden.count, 1);
        assert_eq!(filtered.count, 1);
        assert_eq!(filtered.bytes, 10);
    }
}

pub(super) fn run_tree_children_load(fs: Arc<NativeFs>, path: PathBuf) -> Vec<TreeChild> {
    let mut children: Vec<TreeChild> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(path) {
        for dirent in rd.flatten() {
            let p = dirent.path();
            let Some(name) = p.file_name().and_then(|s| s.to_str()).map(str::to_owned) else {
                continue;
            };
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
            // Platform hidden semantics, same contract as the file
            // list (FileEntry::hidden). This runs on a worker; the
            // extra symlink_metadata is fine here and keeps the
            // render-side filter a pure flag read.
            let hidden = std::fs::symlink_metadata(&p)
                .map(|m| ferail_fs_native::entry_is_hidden(&name, &m))
                .unwrap_or_else(|_| name.starts_with('.'));
            let node_id = fs.id_for_path(&p);
            let has_subdirs = dir_has_subdir(&p);
            // Display leaf for the label (macOS `:` → `/`); `name` already
            // drove the hidden check and `path` drives navigation.
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
    children
}

/// Whether `path` has at least one directory child (symlinks-to-dir
/// count, matching the tree's own child filter). Early-exits on the
/// first hit so the common case touches a handful of dirents; an
/// unreadable directory reports `false` — expanding it would show
/// nothing anyway. Worker-thread only: this is a real read_dir.
pub(super) fn dir_has_subdir(path: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(path) else {
        return false;
    };
    for dirent in rd.flatten() {
        let Ok(ft) = dirent.file_type() else {
            continue;
        };
        if ft.is_dir() {
            return true;
        }
        if ft.is_symlink()
            && std::fs::metadata(dirent.path())
                .map(|m| m.is_dir())
                .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// `x-apple.systempreferences:` URL that deep-links straight to the
/// Full Disk Access pane of Privacy & Security. Unlike Files and
/// Folders, this pane has a "+" button so the user can add Ferail
/// manually rather than waiting for a per-folder TCC prompt.
pub(crate) const FULL_DISK_ACCESS_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// An in-pane error: a title, prose body, and an optional clickable
/// link (label + settings URL) rendered as a separate affordance
/// below the body so only the link itself is interactive.
pub(super) struct ErrorCopy {
    pub title: SharedString,
    pub body: SharedString,
    pub link: Option<(SharedString, &'static str)>,
}

/// Map an `EnumerationError` to error-pane copy. macOS users hitting
/// `Documents` / `Desktop` / `Downloads` for the first time in a
/// sandboxed launcher will see the TCC permission case; other
/// variants get a generic message. The permission case offers a
/// clickable link straight to the Full Disk Access pane, where
/// Ferail can be added with the "+" button.
pub(super) fn error_copy(err: &EnumerationError) -> ErrorCopy {
    match err {
        EnumerationError::PermissionDenied => ErrorCopy {
            title: tr!("Access required"),
            body: tr!(
                "Ferail needs permission to read this folder. The link below \
                   opens Full Disk Access and copies Ferail's path so you can \
                   add it with the \"+\" button."
            ),
            link: Some((
                tr!("Open Full Disk Access settings"),
                FULL_DISK_ACCESS_SETTINGS_URL,
            )),
        },
        EnumerationError::NotFound => ErrorCopy {
            title: tr!("Folder not found"),
            body: tr!("This location may have been moved, renamed, or unmounted."),
            link: None,
        },
        EnumerationError::Other(msg) => ErrorCopy {
            title: tr!("Couldn't open this folder"),
            body: msg.clone().into(),
            link: None,
        },
    }
}

/// Middle-truncate a path so the basename stays visible but the
/// middle is collapsed to an ellipsis. Useful in the preview pane
/// where the full path would otherwise blow out the column width.
/// Falls back to a tail-truncation when the basename alone exceeds
/// `max`. Char-based length counting (handles non-ASCII path
/// components); byte indexing only ever lands on `/` which is ASCII.
pub(super) fn middle_truncate_path(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let basename_start = s.rfind('/').map(|i| i + 1).unwrap_or(0);
    let basename: Vec<char> = s[basename_start..].chars().collect();
    if basename.len() + 3 >= max {
        let take = max.saturating_sub(1);
        let start = basename.len().saturating_sub(take);
        let tail: String = basename[start..].iter().collect();
        return format!("\u{2026}{}", tail);
    }
    let prefix_budget = max - basename.len() - 2;
    let prefix: String = chars[..prefix_budget].iter().collect();
    let bn: String = basename.iter().collect();
    format!("{}\u{2026}/{}", prefix, bn)
}

#[cfg(test)]
mod middle_truncate_tests {
    use super::middle_truncate_path;

    #[test]
    fn short_path_unchanged() {
        assert_eq!(
            middle_truncate_path("/Users/x/file.txt", 40),
            "/Users/x/file.txt"
        );
    }

    #[test]
    fn long_path_keeps_basename() {
        let out = middle_truncate_path("/Users/x/Library/Application Support/Ferail/file.txt", 30);
        assert!(out.ends_with("/file.txt"), "basename preserved: {out}");
        assert!(out.contains('\u{2026}'), "ellipsis inserted: {out}");
    }

    #[test]
    fn very_long_basename_tail_truncates() {
        let s = "/x/this-is-an-absurdly-long-filename-that-blows-past-the-limit.txt";
        let out = middle_truncate_path(s, 20);
        assert!(out.starts_with('\u{2026}'), "leading ellipsis: {out}");
        assert!(out.len() <= 25, "approx max width respected: {out}");
    }
}
