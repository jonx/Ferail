//! Breadcrumb (Cmd+L) path autocomplete.
//!
//! As the user types, a background worker lists the
//! parent directory of the typed prefix and offers matching folder
//! names in the input's completion menu (Up/Down to pick, Enter to
//! accept — the menu owns the keys while it's open). Accepting a
//! suggestion replaces only the partial segment and appends a
//! separator so the user can keep drilling down.
//!
//! Prime directive: callers run directory enumeration on the background
//! executor — the input handler itself never touches the filesystem.

use std::path::PathBuf;

/// Cap on suggestions per keystroke — a directory with thousands of
/// children must not flood the menu (or the worker's sort).
const MAX_SUGGESTIONS: usize = 50;

/// Split the typed text (up to the cursor) at its last path separator
/// into `(directory to enumerate, partial segment to match, UTF-8 byte
/// offset where the segment starts)`. `~` expands for the lookup but
/// the replacement range only covers the segment, so the visible text
/// keeps whatever prefix the user typed. Returns `None` when there's
/// no separator yet — without one we don't know the base directory.
fn split_for_completion(typed: &str) -> Option<(PathBuf, String, usize)> {
    let sep_idx = typed.rfind(['/', '\\'])?;
    let (dir_str, partial) = typed.split_at(sep_idx + 1);
    let dir = if let Some(rest) = dir_str.strip_prefix('~') {
        let mut h = ferail_fs_native::home_dir();
        let rest = rest.trim_start_matches(['/', '\\']);
        if !rest.is_empty() {
            h.push(rest);
        }
        h
    } else {
        PathBuf::from(dir_str)
    };
    let seg_start = dir_str.len();
    Some((dir, partial.to_string(), seg_start))
}

/// Background body: list `dir`, keep child directories whose name
/// starts with `partial` (case-insensitive; dotfolders only offered
/// once the user types a leading `.`), alphabetize, cap.
fn folder_matches(dir: &PathBuf, partial: &str) -> Vec<String> {
    let partial_lower = partial.to_lowercase();
    let want_hidden = partial.starts_with('.');
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            if name.starts_with('.') && !want_hidden {
                return None;
            }
            if !name.to_lowercase().starts_with(&partial_lower) {
                return None;
            }
            // `is_dir` follows symlinks so a link-to-folder still
            // completes — it's a navigation target.
            if !e.path().is_dir() {
                return None;
            }
            Some(name)
        })
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.truncate(MAX_SUGGESTIONS);
    names
}

/// Build path suggestions for Ferail's ordinary single-line address input.
/// This function performs directory I/O and must therefore only be called on
/// a background executor. Replacement ranges use UTF-8 bytes, as InputState
/// does.
pub fn single_line_suggestions(
    value: &str,
    cursor: usize,
) -> Vec<crate::single_line_complete::SingleLineSuggestion> {
    let cursor = cursor.min(value.len());
    if !value.is_char_boundary(cursor) {
        return Vec::new();
    }
    let upto = &value[..cursor];
    let Some((dir, partial, _)) = split_for_completion(upto) else {
        return Vec::new();
    };
    let replacement_start = upto
        .rfind(['/', '\\'])
        .map(|index| index + 1)
        .unwrap_or(cursor);
    folder_matches(&dir, &partial)
        .into_iter()
        .map(|name| crate::single_line_complete::SingleLineSuggestion {
            label: name.clone().into(),
            detail: None,
            replacement: replacement_start..cursor,
            insertion: format!("{name}{}", std::path::MAIN_SEPARATOR).into(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::split_for_completion;

    #[test]
    fn splits_at_last_separator() {
        let (dir, partial, seg_start) = split_for_completion("/Users/jk").unwrap();
        assert_eq!(dir, std::path::PathBuf::from("/Users/"));
        assert_eq!(partial, "jk");
        assert_eq!(seg_start, 7);
    }

    #[test]
    fn root_and_trailing_slash_list_everything() {
        let (dir, partial, seg_start) = split_for_completion("/").unwrap();
        assert_eq!(dir, std::path::PathBuf::from("/"));
        assert_eq!(partial, "");
        assert_eq!(seg_start, 1);

        let (dir, partial, _) = split_for_completion("/Users/").unwrap();
        assert_eq!(dir, std::path::PathBuf::from("/Users/"));
        assert_eq!(partial, "");
    }

    #[test]
    fn tilde_expands_for_lookup_only() {
        let (dir, partial, seg_start) = split_for_completion("~/Doc").unwrap();
        assert_eq!(dir, ferail_fs_native::home_dir());
        assert_eq!(partial, "Doc");
        // Replacement starts after "~/" — the visible `~` is kept.
        assert_eq!(seg_start, 2);
    }

    #[test]
    fn no_separator_means_no_base_directory() {
        assert!(split_for_completion("Doc").is_none());
        assert!(split_for_completion("").is_none());
        assert!(split_for_completion("~").is_none());
    }
}
