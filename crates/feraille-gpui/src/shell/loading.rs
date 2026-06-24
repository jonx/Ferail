use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use feraille_core::{EnumerationError, FileEntry, NodeId};
use feraille_fs_native::{DEFAULT_ENUMERATION_BATCH, NativeFs};

use crate::tree::TreeChild;

pub(crate) struct LoadBatch {
    pub entries: Vec<FileEntry>,
    pub paths: HashMap<NodeId, PathBuf>,
}

pub(super) enum LoadMsg {
    Batch(LoadBatch),
    Done(Option<EnumerationError>),
}

pub(super) fn run_directory_load_streaming(
    fs: Arc<NativeFs>,
    path: PathBuf,
    show_hidden: bool,
    filter_text: String,
    cancel: Arc<AtomicBool>,
    tx: async_channel::Sender<LoadMsg>,
) {
    let needle = filter_text.trim().to_lowercase();
    let error = fs.enumerate_streaming(&path, DEFAULT_ENUMERATION_BATCH, &cancel, |entries| {
        let batch = filter_directory_batch(&fs, entries, show_hidden, &needle);
        if !batch.entries.is_empty() && tx.send_blocking(LoadMsg::Batch(batch)).is_err() {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    let _ = tx.send_blocking(LoadMsg::Done(error));
}

fn filter_directory_batch(
    fs: &NativeFs,
    entries: Vec<FileEntry>,
    show_hidden: bool,
    needle: &str,
) -> LoadBatch {
    let entries: Vec<FileEntry> = entries
        .into_iter()
        // `hidden` carries platform semantics (BSD UF_HIDDEN on macOS,
        // FILE_ATTRIBUTE_HIDDEN on Windows) resolved at enumerate time —
        // never re-derive from the name here.
        .filter(|e| show_hidden || !e.hidden)
        .filter(|e| {
            if needle.is_empty() {
                true
            } else {
                // Filter searches the visible Format value too —
                // otherwise typing "pdf document" or "zip archive"
                // misses rows where the magic-detected text is the
                // only place those phrases appear.
                let (format, _) = e.format_label();
                e.name.to_lowercase().contains(needle) || format.to_lowercase().contains(needle)
            }
        })
        .collect();
    let mut paths = HashMap::with_capacity(entries.len());
    for entry in &entries {
        if let Some(path) = fs.path_for(entry.id) {
            paths.insert(entry.id, path);
        }
    }
    LoadBatch { entries, paths }
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
                .map(|m| feraille_fs_native::entry_is_hidden(&name, &m))
                .unwrap_or_else(|_| name.starts_with('.'));
            let node_id = fs.id_for_path(&p);
            let has_subdirs = dir_has_subdir(&p);
            // Display leaf for the label (macOS `:` → `/`); `name` already
            // drove the hidden check and `path` drives navigation.
            let label = feraille_fs_native::paths::display_leaf(&name).into_owned();
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
/// Folders, this pane has a "+" button so the user can add Feraille
/// manually rather than waiting for a per-folder TCC prompt.
pub(super) const FULL_DISK_ACCESS_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// An in-pane error: a title, prose body, and an optional clickable
/// link (label + settings URL) rendered as a separate affordance
/// below the body so only the link itself is interactive.
pub(super) struct ErrorCopy {
    pub title: &'static str,
    pub body: String,
    pub link: Option<(&'static str, &'static str)>,
}

/// Map an `EnumerationError` to error-pane copy. macOS users hitting
/// `Documents` / `Desktop` / `Downloads` for the first time in a
/// sandboxed launcher will see the TCC permission case; other
/// variants get a generic message. The permission case offers a
/// clickable link straight to the Full Disk Access pane, where
/// Feraille can be added with the "+" button.
pub(super) fn error_copy(err: &EnumerationError) -> ErrorCopy {
    match err {
        EnumerationError::PermissionDenied => ErrorCopy {
            title: "Access required",
            body: "Feraille needs permission to read this folder. The link below \
                   opens Full Disk Access and copies Feraille's path so you can \
                   add it with the \"+\" button."
                .to_string(),
            link: Some((
                "Open Full Disk Access settings",
                FULL_DISK_ACCESS_SETTINGS_URL,
            )),
        },
        EnumerationError::NotFound => ErrorCopy {
            title: "Folder not found",
            body: "This location may have been moved, renamed, or unmounted.".to_string(),
            link: None,
        },
        EnumerationError::Other(msg) => ErrorCopy {
            title: "Couldn't open this folder",
            body: msg.clone(),
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
        let out =
            middle_truncate_path("/Users/x/Library/Application Support/Feraille/file.txt", 30);
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
