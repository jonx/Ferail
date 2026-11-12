//! Verify Integrity: re-hash files whose content hash was recorded earlier
//! and report the ones whose bytes changed while their size and modification
//! time did not, the signature of silent corruption (docs/features/INTEGRITY.md).
//!
//! The hashes come from the duplicate finder's cache ([`DupeHashCache`]),
//! filled by verified copies, Find Duplicates, Compare Folders and earlier
//! runs of this check. Nothing here runs unless the user asked for it: one
//! streaming walk under the selection, cancellable, with constant memory
//! apart from the caller's list of findings.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::dupes::{is_dataless, mtime_unix, CachedHash, DupeHashCache};

const HASH_CHUNK: usize = 1024 * 1024;

/// What one file's check concluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegrityVerdict {
    /// Same size and mtime as recorded, same hash.
    Verified,
    /// Same size and mtime as recorded, different hash: silent corruption.
    Corrupted,
    /// Size or mtime differ from the record: an ordinary edit.
    Modified,
    /// No hash was recorded for this file; one is recorded now.
    Baseline,
}

/// Decide a file's verdict from its stored record, its size and mtime on
/// disk, and the hash just computed. Pure.
pub fn classify(
    record: Option<&CachedHash>,
    size: u64,
    mtime_unix: i64,
    fresh_hex: &str,
) -> IntegrityVerdict {
    match record {
        None => IntegrityVerdict::Baseline,
        Some(r) if r.size != size || r.mtime_unix != mtime_unix => IntegrityVerdict::Modified,
        Some(r) if r.hash.eq_ignore_ascii_case(fresh_hex) => IntegrityVerdict::Verified,
        Some(_) => IntegrityVerdict::Corrupted,
    }
}

/// Totals of a run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IntegrityCounts {
    pub files_checked: u64,
    pub bytes_hashed: u64,
    pub verified: u64,
    pub corrupted: u64,
    pub modified: u64,
    pub baseline: u64,
    /// Files and folders that could not be read.
    pub unreadable: u64,
    /// Files under a location the privacy settings exclude: nothing is
    /// recorded there, so nothing can be checked.
    pub excluded: u64,
}

/// Live counters shared between the worker and the UI, which samples them
/// on its own clock. Relaxed atomics: no lock, no channel on the hot path.
#[derive(Default)]
pub struct IntegrityProgress {
    files_checked: AtomicU64,
    bytes_hashed: AtomicU64,
    verified: AtomicU64,
    corrupted: AtomicU64,
    modified: AtomicU64,
    baseline: AtomicU64,
    unreadable: AtomicU64,
    excluded: AtomicU64,
}

impl IntegrityProgress {
    pub fn snapshot(&self) -> IntegrityCounts {
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        IntegrityCounts {
            files_checked: get(&self.files_checked),
            bytes_hashed: get(&self.bytes_hashed),
            verified: get(&self.verified),
            corrupted: get(&self.corrupted),
            modified: get(&self.modified),
            baseline: get(&self.baseline),
            unreadable: get(&self.unreadable),
            excluded: get(&self.excluded),
        }
    }

    fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// A file whose content changed under an unchanged size and mtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorruptFile {
    pub path: PathBuf,
    pub size: u64,
    /// The BLAKE3 hex recorded earlier.
    pub expected: String,
    /// The BLAKE3 hex the file has now.
    pub actual: String,
}

/// Check every regular file at or under `roots`. Symlinks are not followed,
/// undownloaded cloud placeholders are skipped (reading one would download
/// it), and files under a privacy exclusion are counted but not read.
///
/// A verified, modified or new file has its record refreshed, which also
/// keeps it inside the retention period. A corrupted file's record is left
/// alone, so the next run reports it again. Returns early when `cancel` is
/// raised.
pub fn verify_integrity(
    roots: &[PathBuf],
    cache: &dyn DupeHashCache,
    progress: &IntegrityProgress,
    cancel: &AtomicBool,
    mut on_corrupt: impl FnMut(CorruptFile),
) {
    ferail_core::path_guard::assert_off_ui_thread("verify_integrity");
    let mut buf = vec![0u8; HASH_CHUNK];
    let mut stack: Vec<PathBuf> = Vec::new();
    for root in roots {
        match fs::symlink_metadata(root) {
            Ok(meta) if meta.is_dir() => stack.push(root.clone()),
            Ok(meta) if meta.is_file() => {
                check_file(root, &meta, cache, progress, cancel, &mut buf, &mut on_corrupt)
            }
            Ok(_) => {}
            Err(_) => IntegrityProgress::bump(&progress.unreadable),
        }
        while let Some(dir) = stack.pop() {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                IntegrityProgress::bump(&progress.unreadable);
                continue;
            };
            for entry in entries.flatten() {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let path = entry.path();
                let Ok(meta) = fs::symlink_metadata(&path) else {
                    IntegrityProgress::bump(&progress.unreadable);
                    continue;
                };
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    stack.push(path);
                } else if meta.is_file() {
                    check_file(&path, &meta, cache, progress, cancel, &mut buf, &mut on_corrupt);
                }
            }
        }
    }
}

fn check_file(
    path: &Path,
    meta: &fs::Metadata,
    cache: &dyn DupeHashCache,
    progress: &IntegrityProgress,
    cancel: &AtomicBool,
    buf: &mut [u8],
    on_corrupt: &mut impl FnMut(CorruptFile),
) {
    if is_dataless(meta) {
        return;
    }
    if !cache.records(path) {
        IntegrityProgress::bump(&progress.excluded);
        return;
    }
    let size = meta.len();
    let mtime = mtime_unix(meta);
    let record = cache.get_record(path);
    let fresh = match hash_file(path, buf, progress, cancel) {
        Ok(Some(hash)) => hash,
        Ok(None) => return,
        Err(_) => {
            IntegrityProgress::bump(&progress.files_checked);
            IntegrityProgress::bump(&progress.unreadable);
            return;
        }
    };
    IntegrityProgress::bump(&progress.files_checked);
    // A file written while it was being read is an edit in progress, not
    // corruption, and its hash describes no stable state worth recording.
    let stable = fs::symlink_metadata(path)
        .map(|after| after.len() == size && mtime_unix(&after) == mtime)
        .unwrap_or(false);
    if !stable {
        IntegrityProgress::bump(&progress.modified);
        return;
    }
    match classify(record.as_ref(), size, mtime, &fresh) {
        IntegrityVerdict::Corrupted => {
            IntegrityProgress::bump(&progress.corrupted);
            on_corrupt(CorruptFile {
                path: path.to_path_buf(),
                size,
                expected: record.map(|r| r.hash).unwrap_or_default(),
                actual: fresh,
            });
        }
        verdict => {
            IntegrityProgress::bump(match verdict {
                IntegrityVerdict::Verified => &progress.verified,
                IntegrityVerdict::Modified => &progress.modified,
                _ => &progress.baseline,
            });
            cache.put_full(path, size, mtime, &fresh);
        }
    }
}

/// BLAKE3 hex of `path`, or `None` when cancelled mid-read.
fn hash_file(
    path: &Path,
    buf: &mut [u8],
    progress: &IntegrityProgress,
    cancel: &AtomicBool,
) -> std::io::Result<Option<String>> {
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let n = file.read(buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        progress.bytes_hashed.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(Some(hasher.finalize().to_hex().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MemCache {
        rows: RefCell<HashMap<PathBuf, CachedHash>>,
        excluded: Option<PathBuf>,
    }

    impl DupeHashCache for MemCache {
        fn get_full(&self, path: &Path, size: u64, mtime_unix: i64) -> Option<String> {
            self.rows
                .borrow()
                .get(path)
                .filter(|r| r.size == size && r.mtime_unix == mtime_unix)
                .map(|r| r.hash.clone())
        }
        fn put_full(&self, path: &Path, size: u64, mtime_unix: i64, hash: &str) {
            if !self.records(path) {
                return;
            }
            self.rows.borrow_mut().insert(
                path.to_path_buf(),
                CachedHash {
                    size,
                    mtime_unix,
                    hash: hash.to_string(),
                },
            );
        }
        fn get_record(&self, path: &Path) -> Option<CachedHash> {
            self.rows.borrow().get(path).cloned()
        }
        fn records(&self, path: &Path) -> bool {
            !self.excluded.as_ref().is_some_and(|e| path.starts_with(e))
        }
    }

    fn rec(size: u64, mtime: i64, hash: &str) -> CachedHash {
        CachedHash {
            size,
            mtime_unix: mtime,
            hash: hash.into(),
        }
    }

    #[test]
    fn classify_covers_every_case() {
        assert_eq!(classify(None, 3, 10, "aa"), IntegrityVerdict::Baseline);
        let r = rec(3, 10, "aa");
        assert_eq!(classify(Some(&r), 3, 10, "aa"), IntegrityVerdict::Verified);
        assert_eq!(classify(Some(&r), 3, 10, "AA"), IntegrityVerdict::Verified);
        assert_eq!(classify(Some(&r), 3, 10, "bb"), IntegrityVerdict::Corrupted);
        assert_eq!(classify(Some(&r), 4, 10, "bb"), IntegrityVerdict::Modified);
        assert_eq!(classify(Some(&r), 3, 11, "bb"), IntegrityVerdict::Modified);
        // A legitimate edit that happens to restore the old bytes is still
        // an edit: the key changed.
        assert_eq!(classify(Some(&r), 3, 11, "aa"), IntegrityVerdict::Modified);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ferail-integrity-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn key(p: &Path) -> (u64, i64) {
        let m = fs::symlink_metadata(p).unwrap();
        (m.len(), mtime_unix(&m))
    }

    fn run(roots: &[PathBuf], cache: &MemCache) -> (IntegrityCounts, Vec<CorruptFile>) {
        let progress = IntegrityProgress::default();
        let mut found = Vec::new();
        verify_integrity(roots, cache, &progress, &AtomicBool::new(false), |c| {
            found.push(c)
        });
        (progress.snapshot(), found)
    }

    /// Rewrite a file's bytes and put its mtime back: what bit rot looks like
    /// from the filesystem's side.
    fn rot(p: &Path) {
        let before = fs::metadata(p).unwrap().modified().unwrap();
        let mut bytes = fs::read(p).unwrap();
        bytes[0] ^= 0x01;
        fs::write(p, &bytes).unwrap();
        fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(before)
            .unwrap();
    }

    #[test]
    fn each_case_is_classified_on_disk() {
        let root = scratch("cases");
        let ok = root.join("ok.txt");
        let rotten = root.join("sub/rotten.txt");
        let edited = root.join("edited.txt");
        let fresh = root.join("new.txt");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(&ok, b"stable").unwrap();
        fs::write(&rotten, b"will rot").unwrap();
        fs::write(&edited, b"draft").unwrap();
        fs::write(&fresh, b"never seen").unwrap();

        let cache = MemCache::default();
        for p in [&ok, &rotten, &edited] {
            let (size, mtime) = key(p);
            let hash = blake3::hash(&fs::read(p).unwrap()).to_hex().to_string();
            cache.put_full(p, size, mtime, &hash);
        }
        rot(&rotten);
        fs::write(&edited, b"final version").unwrap();

        let (counts, found) = run(std::slice::from_ref(&root), &cache);
        assert_eq!(counts.files_checked, 4);
        assert_eq!(counts.verified, 1);
        assert_eq!(counts.corrupted, 1);
        assert_eq!(counts.modified, 1);
        assert_eq!(counts.baseline, 1);
        assert_eq!(counts.unreadable, 0);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, rotten);
        assert_ne!(found[0].expected, found[0].actual);

        // The corrupted record is kept, so a second run reports it again;
        // the edit and the new file now have baselines and verify.
        let (again, found) = run(std::slice::from_ref(&root), &cache);
        assert_eq!(again.corrupted, 1);
        assert_eq!(found[0].path, rotten);
        assert_eq!(again.verified, 3);
        assert_eq!(again.modified + again.baseline, 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn excluded_files_are_counted_not_read() {
        let root = scratch("excluded");
        fs::create_dir_all(root.join("private")).unwrap();
        fs::write(root.join("private/secret.txt"), b"x").unwrap();
        fs::write(root.join("public.txt"), b"y").unwrap();
        let cache = MemCache {
            excluded: Some(root.join("private")),
            ..Default::default()
        };
        let (counts, _) = run(std::slice::from_ref(&root), &cache);
        assert_eq!(counts.excluded, 1);
        assert_eq!(counts.baseline, 1);
        assert_eq!(counts.files_checked, 1);
        assert!(cache.get_record(&root.join("private/secret.txt")).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_selected_file_and_a_missing_path() {
        let root = scratch("roots");
        let file = root.join("one.txt");
        fs::write(&file, b"one").unwrap();
        let cache = MemCache::default();
        let (counts, _) = run(&[file.clone(), root.join("gone")], &cache);
        assert_eq!(counts.baseline, 1);
        assert_eq!(counts.unreadable, 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let root = scratch("links");
        fs::create_dir_all(root.join("real")).unwrap();
        fs::write(root.join("real/a.txt"), b"a").unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let cache = MemCache::default();
        let (counts, _) = run(std::slice::from_ref(&root), &cache);
        assert_eq!(counts.files_checked, 1);
        let _ = fs::remove_dir_all(&root);
    }
}
