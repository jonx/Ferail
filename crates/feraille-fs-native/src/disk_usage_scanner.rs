//! Disk-usage worker. Walks a directory tree depth-first via `std::fs`,
//! emits a stream of [`DiskUsageFact`]s in batches, and reports progress
//! at a throttled cadence. Pure-function: returns `Some(error)` only on
//! hard failure of the top-level `read_dir`; per-subdir permission errors
//! are absorbed so the scan reports a partial-but-complete tree.
//!
//! Mirrors the shape of [`NativeFs::enumerate_streaming`] — same cancel
//! flag, same batching cadence, same callback model. The host
//! application owns the thread spawn + event-loop dispatch.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime};

use feraille_core::EnumerationError;
use feraille_disk_usage::{
    classify_path, DiskUsageFact, DiskUsageStats, FileCategory, NodeKind,
};

use crate::{map_io_error, NativeFs};

/// Default fact-batch size, mirroring `DEFAULT_ENUMERATION_BATCH`.
pub const DEFAULT_DU_BATCH: usize = 256;
/// Minimum gap between progress callbacks.
const PROGRESS_THROTTLE_MS: u128 = 250;

impl NativeFs {
    /// Recursive depth-first scan of `root`. Emits [`DiskUsageFact`]s in
    /// batches of up to `batch_size`, calling `on_batch` whenever the
    /// buffer fills or the scan finishes. `on_progress` is invoked at
    /// most once per ~250 ms with running totals.
    ///
    /// `cancel` is checked between dirents and at each level boundary;
    /// when set, the in-flight buffer is flushed and the scan returns
    /// without touching the rest of the tree.
    ///
    /// `descend_packages` controls whether macOS package directories
    /// (`*.app`, `*.bundle`, `*.framework`, etc.) are treated as opaque
    /// leaves (default, `false`) or descended into. The leaf path
    /// classifies them as [`FileCategory::Executable`] / `Other` and
    /// reports their immediate-`metadata` size only — proper bundle
    /// totals via a child-only sum land in iter-6.4.
    ///
    /// Symlinks: walked via `symlink_metadata`, never followed; counted
    /// as 0-byte leaves to keep the walk cycle-safe.
    pub fn scan_disk_usage(
        &self,
        root: &Path,
        batch_size: usize,
        cancel: &AtomicBool,
        descend_packages: bool,
        mut on_batch: impl FnMut(Vec<DiskUsageFact>),
        mut on_progress: impl FnMut(DiskUsageStats),
    ) -> Option<EnumerationError> {
        let canonical_root = match fs::canonicalize(root) {
            Ok(p) => p,
            Err(e) => return Some(map_io_error(&e)),
        };
        // Up-front check that the root is a directory we can read; mirrors
        // enumerate_streaming so callers see the same error class.
        match fs::read_dir(&canonical_root) {
            Ok(rd) => drop(rd),
            Err(e) => return Some(map_io_error(&e)),
        }

        let root_id = self.id_for_path(&canonical_root);
        let mut buffer: Vec<DiskUsageFact> = Vec::with_capacity(batch_size);
        let mut stats = DiskUsageStats::default();
        let mut last_progress = Instant::now();

        // Discover the root container itself so the tree has a kind/name.
        let root_name = canonical_root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned();
        let root_mtime = fs::symlink_metadata(&canonical_root)
            .ok()
            .and_then(|m| m.modified().ok());
        let root_is_cloud = is_icloud_path(&canonical_root);
        buffer.push(DiskUsageFact::NodeDiscovered {
            node: root_id,
            kind: NodeKind::Container,
            file_category: FileCategory::Other,
            mtime: root_mtime,
            name: root_name,
            is_cloud: root_is_cloud,
        });

        // DFS stack of (container path, container node id).
        let mut stack: Vec<(PathBuf, feraille_core::NodeId)> = vec![(canonical_root, root_id)];

        while let Some((dir_path, dir_id)) = stack.pop() {
            if cancel.load(Ordering::Relaxed) {
                if !buffer.is_empty() {
                    on_batch(std::mem::take(&mut buffer));
                }
                return None;
            }

            buffer.push(DiskUsageFact::ContainerScanStarted { container: dir_id });
            stats.dirs_scanned = stats.dirs_scanned.saturating_add(1);

            let read_dir = match fs::read_dir(&dir_path) {
                Ok(rd) => rd,
                Err(_) => {
                    // Permission denied or transient I/O — skip the dir
                    // body but still mark it complete so the UI doesn't
                    // see "Scanning" forever.
                    buffer.push(DiskUsageFact::ContainerScanCompleted { container: dir_id });
                    if buffer.len() >= batch_size {
                        on_batch(std::mem::take(&mut buffer));
                        buffer.reserve(batch_size);
                    }
                    continue;
                }
            };

            for dirent in read_dir.flatten() {
                if cancel.load(Ordering::Relaxed) {
                    if !buffer.is_empty() {
                        on_batch(std::mem::take(&mut buffer));
                    }
                    return None;
                }

                let child_path = dirent.path();
                let Some(name) = child_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(str::to_owned)
                else {
                    continue;
                };

                let metadata = match fs::symlink_metadata(&child_path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let ft = metadata.file_type();
                let is_dir = ft.is_dir() && !ft.is_symlink();
                let mtime = metadata.modified().ok();
                let child_id = self.id_for_path(&child_path);

                let mac_pkg = is_mac_package(&child_path);
                let treat_as_leaf = !is_dir || (mac_pkg && !descend_packages);

                let (kind, file_category, size) = if treat_as_leaf {
                    let fc = if mac_pkg {
                        FileCategory::Executable
                    } else {
                        classify_path(&child_path)
                    };
                    let size = if ft.is_symlink() {
                        0
                    } else if mac_pkg && !descend_packages {
                        // Bundle as opaque leaf — but we still want a
                        // Finder-style rolled-up total, not the
                        // useless inode-stat size. Walk the package
                        // contents and sum them.
                        recursive_size(&child_path, cancel)
                    } else {
                        metadata.len()
                    };
                    (NodeKind::File, fc, size)
                } else {
                    (NodeKind::Container, FileCategory::Other, 0u64)
                };

                let child_is_cloud = root_is_cloud || is_icloud_path(&child_path);
                buffer.push(DiskUsageFact::NodeDiscovered {
                    node: child_id,
                    kind,
                    file_category,
                    mtime,
                    name,
                    is_cloud: child_is_cloud,
                });
                buffer.push(DiskUsageFact::NodeLinked {
                    container: dir_id,
                    node: child_id,
                });
                if size > 0 {
                    buffer.push(DiskUsageFact::NodeSizeAdded {
                        node: child_id,
                        size_bytes: size,
                    });
                }
                // Allocated size on macOS comes from the block count.
                // A symlink reports 0; a tiny file reports the 4 KB
                // block tax; a sparse file reports much less than its
                // apparent size. Falls back to apparent on platforms
                // that don't expose block counts.
                let allocated = allocated_size(&metadata);
                if allocated > 0 {
                    buffer.push(DiskUsageFact::NodeAllocatedAdded {
                        node: child_id,
                        bytes: allocated,
                    });
                }

                if matches!(kind, NodeKind::File) {
                    stats.files_scanned = stats.files_scanned.saturating_add(1);
                    stats.bytes_scanned = stats.bytes_scanned.saturating_add(size);
                } else {
                    // Will be popped off the stack and entered later.
                    stack.push((child_path, child_id));
                }

                if buffer.len() >= batch_size {
                    on_batch(std::mem::take(&mut buffer));
                    buffer.reserve(batch_size);
                }
            }

            buffer.push(DiskUsageFact::ContainerScanCompleted { container: dir_id });
            if buffer.len() >= batch_size {
                on_batch(std::mem::take(&mut buffer));
                buffer.reserve(batch_size);
            }

            if last_progress.elapsed().as_millis() >= PROGRESS_THROTTLE_MS {
                on_progress(stats);
                last_progress = Instant::now();
            }
        }

        if !buffer.is_empty() {
            on_batch(buffer);
        }
        on_progress(stats);
        None
    }
}

/// Sum every regular file under `root` — logical bytes
/// (`metadata.len()`), the same semantic Finder's "Size" column uses.
/// Serves two callers: bundle rolled-up sizes inside the disk-usage
/// scan, and the file-list folder-size worker in `feraille-gpui`.
///
/// Iterative DFS, `symlink_metadata` only (no follow), absorbs
/// per-subdir read failures (returns whatever was summed before the
/// failure). `cancel` is checked between dirents; on cancel returns
/// the partial sum, which callers must treat as invalid (don't cache).
pub fn recursive_size(root: &Path, cancel: &AtomicBool) -> u64 {
    let mut total: u64 = 0;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return total;
        }
        let read_dir = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for dirent in read_dir.flatten() {
            if cancel.load(Ordering::Relaxed) {
                return total;
            }
            // Read the metadata captured during directory enumeration instead of
            // re-`stat`ing each path. This is not just faster: on Windows
            // `DirEntry::metadata()` returns the cached `WIN32_FIND_DATA` from the
            // enumeration with NO file open, whereas `fs::symlink_metadata(path)`
            // opens a handle per file — which on a OneDrive / cloud folder makes
            // Windows hydrate (download) every placeholder we touch, exactly the
            // behavior we must never trigger (see the Prime Directive in
            // CLAUDE.md). The logical size is already in the find data, so the
            // folder total is correct without pulling a byte from the cloud.
            // `DirEntry::metadata()` does not follow symlinks, matching the
            // previous `symlink_metadata` semantics.
            let meta = match dirent.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let ft = meta.file_type();
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(dirent.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// On-disk allocated size for a regular file. macOS / Unix exposes
/// this via `MetadataExt::blocks() * 512` (block size is fixed at
/// 512 in the stat man page regardless of the underlying FS block
/// size). Returns 0 on platforms that don't surface block counts.
#[cfg(unix)]
fn allocated_size(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    if meta.file_type().is_symlink() {
        return 0;
    }
    meta.blocks().saturating_mul(512)
}
#[cfg(not(unix))]
fn allocated_size(meta: &fs::Metadata) -> u64 {
    let _ = meta;
    0
}

/// Coarse iCloud-detection by path prefix — macOS stores all
/// ubiquity-managed files under `~/Library/Mobile Documents/`. Cheap
/// (a string starts_with), no NSURL call per file. Doesn't tell us
/// whether a given file is a downloaded copy vs a placeholder; the
/// renderer just paints a cloud glyph either way.
pub(crate) fn is_icloud_path(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let home = std::path::PathBuf::from(home);
            return path.starts_with(home.join("Library/Mobile Documents"));
        }
        false
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

/// macOS package detection by extension. Stays in sync with the
/// classify_extension list so categorization and descent agree.
pub(crate) fn is_mac_package(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "app" | "bundle" | "framework" | "plugin" | "kext" | "xcodeproj"
    )
}

/// Filter helper for callers that want a single mtime as `SystemTime`
/// without taking a dep on `feraille-disk-usage` directly.
#[allow(dead_code)]
pub fn mtime_of(meta: &fs::Metadata) -> Option<SystemTime> {
    meta.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    /// Build a small fixture: tmp/{a.txt(10B), sub/{b.txt(20B), c.png(30B)}}
    fn fixture() -> tempdir_lite::TempDir {
        let tmp = tempdir_lite::TempDir::new("dufix").expect("tmpdir");
        let root = tmp.path();
        let mut f = File::create(root.join("a.txt")).unwrap();
        f.write_all(&[0u8; 10]).unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        let mut f = File::create(root.join("sub").join("b.txt")).unwrap();
        f.write_all(&[0u8; 20]).unwrap();
        let mut f = File::create(root.join("sub").join("c.png")).unwrap();
        f.write_all(&[0u8; 30]).unwrap();
        tmp
    }

    #[test]
    fn scan_emits_facts_and_aggregates_via_apply() {
        let tmp = fixture();
        let fs_native = NativeFs::new();
        let cancel = AtomicBool::new(false);
        let mut all = Vec::new();
        let err = fs_native.scan_disk_usage(
            tmp.path(),
            DEFAULT_DU_BATCH,
            &cancel,
            false,
            |batch| all.extend(batch),
            |_| {},
        );
        assert!(err.is_none());

        let canonical = fs::canonicalize(tmp.path()).unwrap();
        let root_id = fs_native.id_for_path(&canonical);
        let mut tree = feraille_disk_usage::DiskUsageTree::new(root_id);
        tree.apply_facts(&all);

        let layout = feraille_disk_usage::build_layout_node(&tree, root_id, 4);
        // 10 + 20 + 30 = 60 bytes total.
        assert_eq!(layout.size_bytes, 60);
        // Children: a.txt (10) + sub (50). Sorted descending → sub first.
        assert_eq!(layout.children.len(), 2);
        assert_eq!(layout.children[0].size_bytes, 50);
        assert_eq!(layout.children[0].kind, NodeKind::Container);
    }

    #[test]
    fn recursive_size_sums_all_files() {
        let tmp = fixture();
        let cancel = AtomicBool::new(false);
        assert_eq!(recursive_size(tmp.path(), &cancel), 60);
    }

    #[test]
    fn recursive_size_cancel_returns_early() {
        let tmp = fixture();
        let cancel = AtomicBool::new(true);
        // Cancelled before entering the root: nothing summed.
        assert_eq!(recursive_size(tmp.path(), &cancel), 0);
    }

    #[test]
    fn scan_top_level_missing_returns_error() {
        let fs_native = NativeFs::new();
        let cancel = AtomicBool::new(false);
        let err = fs_native.scan_disk_usage(
            Path::new("/this/path/does/not/exist/we/hope"),
            DEFAULT_DU_BATCH,
            &cancel,
            false,
            |_| {},
            |_| {},
        );
        assert!(matches!(
            err,
            Some(EnumerationError::NotFound) | Some(EnumerationError::Other(_))
        ));
    }

    #[test]
    fn scan_respects_cancel_mid_walk() {
        let tmp = fixture();
        let fs_native = NativeFs::new();
        let cancel = AtomicBool::new(true);
        let mut batches = 0usize;
        let err = fs_native.scan_disk_usage(
            tmp.path(),
            DEFAULT_DU_BATCH,
            &cancel,
            false,
            |_| batches += 1,
            |_| {},
        );
        assert!(err.is_none());
        // Cancelled before entering: at most a small partial flush.
        assert!(batches <= 1);
    }
}

/// Tiny in-process tempdir helper so we don't add `tempfile` to the dep
/// list just for unit tests. Drops the directory on `Drop`.
#[cfg(test)]
mod tempdir_lite {
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        pub fn new(prefix: &str) -> io::Result<Self> {
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("feraille-{prefix}-{pid}-{nanos}-{n}"));
            fs::create_dir(&path)?;
            Ok(Self { path })
        }

        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
