//! Compare Folders worker (docs/features/COMPARE_FOLDERS.md).
//!
//! Pairs the direct children of two folders by name and decides, for each
//! name present on both sides, whether the two items hold the same content.
//! The cheap verdicts come from the listing alone: a name on one side only,
//! a file against a folder, two files of different sizes. Everything else
//! needs a read: two equal-sized files are hashed with the duplicate
//! finder's BLAKE3 pipeline and cache ([`crate::file_content_hash`]), two
//! folders are walked recursively and stop at the first difference, two
//! symlinks compare their targets.
//!
//! A verdict is never guessed. A file that cannot be read, an undownloaded
//! cloud placeholder or a cancelled check yields [`CompareState::Unknown`],
//! not `Identical`.
//!
//! `.DS_Store` is Finder's per-folder view state, rewritten whenever a
//! window opens a folder; it is skipped at every level so it cannot make
//! two folders with the same files read as different.

use std::cmp::Ordering as CmpOrdering;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use ferail_core::EntryKind;

use crate::directory_reader::{self, DirectoryEntry};
use crate::dupes::{file_content_hash, DupeHashCache};

/// Names the comparison ignores at every level.
const IGNORED_NAMES: &[&str] = &[".DS_Store"];

/// One child of a compared folder, as the listing reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareItem {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_unix: i64,
    /// Undownloaded cloud placeholder: reading it would start a download,
    /// so its content is never checked.
    pub dataless: bool,
}

/// Where a name stands after comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompareState {
    /// Present in the left (first) folder only.
    OnlyLeft,
    /// Present in the right (second) folder only.
    OnlyRight,
    /// Present on both sides; the content check has not answered yet.
    Pending,
    /// Present on both sides with different content or of different kinds.
    Different,
    /// Present on both sides with the same content, checked byte for byte
    /// through a full hash or a recursive walk.
    Identical,
    /// Present on both sides, but the content could not be checked.
    Unknown,
}

/// One name of the union of both listings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComparePair {
    pub name: String,
    pub left: Option<CompareItem>,
    pub right: Option<CompareItem>,
    pub state: CompareState,
}

fn mtime_unix(entry: &DirectoryEntry) -> i64 {
    entry
        .mtime
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn is_dataless(entry: &DirectoryEntry) -> bool {
    const SF_DATALESS: u32 = 0x4000_0000;
    entry.flags & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
fn is_dataless(_entry: &DirectoryEntry) -> bool {
    false
}

/// The children of `dir`, minus [`IGNORED_NAMES`], and minus hidden entries
/// unless `include_hidden`. Blocking: call it from a worker.
pub fn list_compare_items(
    dir: &Path,
    include_hidden: bool,
    cancel: &AtomicBool,
) -> std::io::Result<Vec<CompareItem>> {
    ferail_core::path_guard::assert_off_ui_thread("folder_compare::list_compare_items");
    let mut items = Vec::new();
    directory_reader::for_each(dir, cancel, |entry| {
        if IGNORED_NAMES.contains(&entry.name.as_str()) || (!include_hidden && entry.hidden) {
            return true;
        }
        items.push(CompareItem {
            name: entry.name.clone(),
            kind: entry.kind,
            size: entry.size,
            mtime_unix: mtime_unix(&entry),
            dataless: is_dataless(&entry),
        });
        true
    })?;
    Ok(items)
}

/// Pair two listings by name and settle every verdict the listings alone
/// can give. Names present on both sides that need a content check come
/// back [`CompareState::Pending`]. Sorted by name.
pub fn pair_listings(mut left: Vec<CompareItem>, mut right: Vec<CompareItem>) -> Vec<ComparePair> {
    left.sort_by(|a, b| a.name.cmp(&b.name));
    right.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = Vec::with_capacity(left.len().max(right.len()));
    let mut l = left.into_iter().peekable();
    let mut r = right.into_iter().peekable();
    loop {
        let order = match (l.peek(), r.peek()) {
            (None, None) => break,
            (Some(_), None) => CmpOrdering::Less,
            (None, Some(_)) => CmpOrdering::Greater,
            (Some(a), Some(b)) => a.name.cmp(&b.name),
        };
        match order {
            CmpOrdering::Less => {
                let item = l.next().expect("peeked");
                out.push(ComparePair {
                    name: item.name.clone(),
                    left: Some(item),
                    right: None,
                    state: CompareState::OnlyLeft,
                });
            }
            CmpOrdering::Greater => {
                let item = r.next().expect("peeked");
                out.push(ComparePair {
                    name: item.name.clone(),
                    left: None,
                    right: Some(item),
                    state: CompareState::OnlyRight,
                });
            }
            CmpOrdering::Equal => {
                let a = l.next().expect("peeked");
                let b = r.next().expect("peeked");
                let state = listing_verdict(&a, &b);
                out.push(ComparePair {
                    name: a.name.clone(),
                    left: Some(a),
                    right: Some(b),
                    state,
                });
            }
        }
    }
    out
}

/// What the listing alone says about two items of the same name.
fn listing_verdict(a: &CompareItem, b: &CompareItem) -> CompareState {
    if a.kind != b.kind {
        return CompareState::Different;
    }
    match a.kind {
        EntryKind::File if a.size != b.size => CompareState::Different,
        EntryKind::File if a.dataless || b.dataless => CompareState::Unknown,
        _ => CompareState::Pending,
    }
}

/// Settle a [`CompareState::Pending`] pair by reading content: hash two
/// files, compare two link targets, walk two folders. Any other state is
/// returned unchanged. Blocking: call it from a worker.
pub fn verify_pair(
    left_dir: &Path,
    right_dir: &Path,
    pair: &ComparePair,
    cache: Option<&dyn DupeHashCache>,
    cancel: &AtomicBool,
) -> CompareState {
    if pair.state != CompareState::Pending {
        return pair.state;
    }
    let (Some(a), Some(b)) = (&pair.left, &pair.right) else {
        return pair.state;
    };
    ferail_core::path_guard::assert_off_ui_thread("folder_compare::verify_pair");
    let left = left_dir.join(&pair.name);
    let right = right_dir.join(&pair.name);
    let verdict = match a.kind {
        EntryKind::File => files_match(&left, a, &right, b, cache, cancel),
        EntryKind::Symlink => links_match(&left, &right),
        EntryKind::Directory => dirs_match(&left, &right, cache, cancel),
    };
    match verdict {
        Some(true) => CompareState::Identical,
        Some(false) => CompareState::Different,
        None => CompareState::Unknown,
    }
}

fn files_match(
    left: &Path,
    a: &CompareItem,
    right: &Path,
    b: &CompareItem,
    cache: Option<&dyn DupeHashCache>,
    cancel: &AtomicBool,
) -> Option<bool> {
    if a.size != b.size {
        return Some(false);
    }
    if a.dataless || b.dataless {
        return None;
    }
    let ha = file_content_hash(left, a.size, a.mtime_unix, cache, cancel)?;
    let hb = file_content_hash(right, b.size, b.mtime_unix, cache, cancel)?;
    Some(ha == hb)
}

fn links_match(left: &Path, right: &Path) -> Option<bool> {
    let a = std::fs::read_link(left).ok()?;
    let b = std::fs::read_link(right).ok()?;
    Some(a == b)
}

/// Recursive content equality of two folders. `Some(false)` at the first
/// difference found, `Some(true)` only when every entry was checked and
/// matched, `None` when something could not be read or `cancel` was raised.
/// Structure is checked before any file is read, so a missing name answers
/// without hashing.
fn dirs_match(
    left: &Path,
    right: &Path,
    cache: Option<&dyn DupeHashCache>,
    cancel: &AtomicBool,
) -> Option<bool> {
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let a = list_compare_items(left, true, cancel).ok()?;
    let b = list_compare_items(right, true, cancel).ok()?;
    // A listing cut short by the cancel flag is partial: it must not read
    // as a missing name.
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let pairs = pair_listings(a, b);
    if pairs.iter().any(|p| {
        matches!(
            p.state,
            CompareState::OnlyLeft | CompareState::OnlyRight | CompareState::Different
        )
    }) {
        return Some(false);
    }
    let mut unknown = pairs.iter().any(|p| p.state == CompareState::Unknown);
    // Files and links at this level first, then descend: a difference near
    // the top answers before a deep subtree is read.
    let (dirs, leaves): (Vec<_>, Vec<_>) = pairs
        .iter()
        .filter(|p| p.state == CompareState::Pending)
        .partition(|p| p.left.as_ref().is_some_and(|i| i.kind == EntryKind::Directory));
    for pair in leaves.into_iter().chain(dirs) {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        match verify_pair(left, right, pair, cache, cancel) {
            CompareState::Identical => {}
            CompareState::Different => return Some(false),
            _ => unknown = true,
        }
    }
    (!unknown).then_some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU32;

    fn item(name: &str, kind: EntryKind, size: u64) -> CompareItem {
        CompareItem {
            name: name.into(),
            kind,
            size,
            mtime_unix: 0,
            dataless: false,
        }
    }

    fn state_of(pairs: &[ComparePair], name: &str) -> CompareState {
        pairs.iter().find(|p| p.name == name).unwrap().state
    }

    #[test]
    fn pairing_settles_what_the_listing_can() {
        let left = vec![
            item("only-left.txt", EntryKind::File, 1),
            item("same-size.txt", EntryKind::File, 5),
            item("size-differs.txt", EntryKind::File, 5),
            item("kind-differs", EntryKind::File, 5),
            item("folder", EntryKind::Directory, 0),
        ];
        let right = vec![
            item("folder", EntryKind::Directory, 0),
            item("kind-differs", EntryKind::Directory, 0),
            item("size-differs.txt", EntryKind::File, 6),
            item("same-size.txt", EntryKind::File, 5),
            item("only-right.txt", EntryKind::File, 1),
        ];
        let pairs = pair_listings(left, right);
        assert_eq!(pairs.len(), 6);
        assert!(pairs.windows(2).all(|w| w[0].name < w[1].name), "sorted");
        assert_eq!(state_of(&pairs, "only-left.txt"), CompareState::OnlyLeft);
        assert_eq!(state_of(&pairs, "only-right.txt"), CompareState::OnlyRight);
        assert_eq!(state_of(&pairs, "same-size.txt"), CompareState::Pending);
        assert_eq!(state_of(&pairs, "size-differs.txt"), CompareState::Different);
        assert_eq!(state_of(&pairs, "kind-differs"), CompareState::Different);
        assert_eq!(state_of(&pairs, "folder"), CompareState::Pending);
    }

    #[test]
    fn a_cloud_placeholder_is_never_called_identical() {
        let mut a = item("x", EntryKind::File, 5);
        a.dataless = true;
        let pairs = pair_listings(vec![a], vec![item("x", EntryKind::File, 5)]);
        assert_eq!(pairs[0].state, CompareState::Unknown);
    }

    struct Fixture {
        root: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl Fixture {
        fn new() -> Self {
            static SEQ: AtomicU32 = AtomicU32::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "ferail-comparefix-{}-{}",
                std::process::id(),
                n
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("l")).unwrap();
            std::fs::create_dir_all(root.join("r")).unwrap();
            Self { root }
        }

        fn write(&self, rel: &str, bytes: &[u8]) {
            let p = self.root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::File::create(&p)
                .unwrap()
                .write_all(bytes)
                .unwrap();
        }

        fn compare(&self) -> Vec<ComparePair> {
            let cancel = AtomicBool::new(false);
            let (l, r) = (self.root.join("l"), self.root.join("r"));
            let a = list_compare_items(&l, true, &cancel).unwrap();
            let b = list_compare_items(&r, true, &cancel).unwrap();
            let mut pairs = pair_listings(a, b);
            for p in pairs.iter_mut() {
                p.state = verify_pair(&l, &r, p, None, &cancel);
            }
            pairs
        }
    }

    #[test]
    fn content_check_tells_equal_sized_files_apart() {
        let f = Fixture::new();
        f.write("l/same.txt", b"hello");
        f.write("r/same.txt", b"hello");
        f.write("l/edited.txt", b"hello");
        f.write("r/edited.txt", b"jello");
        let pairs = f.compare();
        assert_eq!(state_of(&pairs, "same.txt"), CompareState::Identical);
        assert_eq!(state_of(&pairs, "edited.txt"), CompareState::Different);
    }

    #[test]
    fn folders_compare_recursively_and_ignore_finder_state() {
        let f = Fixture::new();
        f.write("l/a/deep/x.bin", b"12345");
        f.write("r/a/deep/x.bin", b"12345");
        f.write("l/a/.DS_Store", b"left view state");
        f.write("l/b/x.bin", b"12345");
        f.write("r/b/x.bin", b"12346");
        f.write("l/c/x.bin", b"1");
        f.write("r/c/x.bin", b"1");
        f.write("r/c/extra.bin", b"2");
        let pairs = f.compare();
        assert_eq!(state_of(&pairs, "a"), CompareState::Identical);
        assert_eq!(state_of(&pairs, "b"), CompareState::Different);
        assert_eq!(state_of(&pairs, "c"), CompareState::Different);
    }

    #[test]
    fn hidden_entries_follow_the_flag() {
        let f = Fixture::new();
        f.write("l/.hidden", b"x");
        f.write("l/shown", b"x");
        let cancel = AtomicBool::new(false);
        let names = |hidden| {
            list_compare_items(&f.root.join("l"), hidden, &cancel)
                .unwrap()
                .into_iter()
                .map(|i| i.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(false), vec!["shown".to_string()]);
        assert_eq!(names(true).len(), 2);
    }

    #[test]
    fn a_cancelled_check_is_unknown_not_identical() {
        let f = Fixture::new();
        f.write("l/x.txt", b"same");
        f.write("r/x.txt", b"same");
        let cancel = AtomicBool::new(true);
        let (l, r) = (f.root.join("l"), f.root.join("r"));
        let pairs = pair_listings(
            vec![item("x.txt", EntryKind::File, 4)],
            vec![item("x.txt", EntryKind::File, 4)],
        );
        assert_eq!(
            verify_pair(&l, &r, &pairs[0], None, &cancel),
            CompareState::Unknown
        );
    }
}
