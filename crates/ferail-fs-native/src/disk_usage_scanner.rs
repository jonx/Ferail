//! Disk-usage worker. Walks a directory tree depth-first via `std::fs`,
//! emits a stream of [`DiskUsageFact`]s in batches, and reports progress
//! at a throttled cadence. Pure-function: returns `Some(error)` only on
//! hard failure of the top-level `read_dir`; per-subdir permission errors
//! are absorbed so the scan reports a partial-but-complete tree.
//!
//! Mirrors the shape of [`NativeFs::enumerate_streaming`], same cancel
//! flag, same batching cadence, same callback model. The host
//! application owns the thread spawn + event-loop dispatch.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Instant, SystemTime};

use ferail_core::EnumerationError;
use ferail_disk_usage::{classify_path, DiskUsageFact, DiskUsageStats, FileCategory, NodeKind};

use crate::{map_io_error, NativeFs};

/// Default fact-batch size, mirroring `DEFAULT_ENUMERATION_BATCH`.
pub const DEFAULT_DU_BATCH: usize = 256;
/// Minimum gap between progress callbacks.
const PROGRESS_THROTTLE_MS: u128 = 250;

struct DuDirectory {
    path: PathBuf,
    id: ferail_core::NodeId,
    device: Option<u64>,
}

impl crate::directory_reader::DirectoryContext for DuDirectory {
    fn directory_path(&self) -> &Path {
        &self.path
    }
}

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
    /// reports their immediate-`metadata` size only: proper bundle
    /// totals via a child-only sum land in iter-6.4.
    ///
    /// Symlinks: walked via `symlink_metadata`, never followed; counted
    /// as 0-byte leaves to keep the walk cycle-safe.
    ///
    /// Counting correctness (Unix):
    /// - **Filesystem boundaries** are not crossed: a directory whose
    ///   `st_dev` differs from its parent's is a mount point and is
    ///   emitted as a 0-byte stub instead of walked (`du -x`
    ///   semantics). Without this, scanning `/` rolled every disk
    ///   under `/Volumes` (and network mounts: hang-prone) into the
    ///   boot-disk number. macOS **firmlinks** (`/Users`,
    ///   `/Applications`, … per `/usr/share/firmlinks`) are the one
    ///   sanctioned crossing: they're how the merged system/data view
    ///   works, and users expect them counted. Their
    ///   `/System/Volumes/Data/...` twins then land on the boundary
    ///   rule, so nothing is walked twice.
    /// - **Hardlinks** (`st_nlink > 1`) are counted once via a
    ///   `(dev, ino)` set: `cp -al` backups and Homebrew Cellars
    ///   otherwise report N× their real size.
    pub fn scan_disk_usage(
        &self,
        root: &Path,
        batch_size: usize,
        cancel: &AtomicBool,
        descend_packages: bool,
        on_batch: impl FnMut(Vec<DiskUsageFact>),
        on_progress: impl FnMut(DiskUsageStats),
    ) -> Option<EnumerationError> {
        self.scan_disk_usage_with_identity(
            root,
            batch_size,
            cancel,
            descend_packages,
            |path| self.id_for_path(path),
            on_batch,
            on_progress,
        )
    }

    /// Scan with identities owned by the caller rather than inserting every
    /// discovered path into [`NativeFs`]'s process-lifetime path maps.
    ///
    /// `id_base + 1` is the root and every subsequent discovery increments the
    /// low part of that scan-local namespace. The consumer can therefore keep
    /// a compact parent-index arena and release the entire scan when its result
    /// surface closes. `id_base` must come from a namespace disjoint from
    /// ordinary `NativeFs` ids (the GPUI Disk Usage surface reserves bit 62).
    #[allow(clippy::too_many_arguments)]
    pub fn scan_disk_usage_local(
        &self,
        root: &Path,
        batch_size: usize,
        cancel: &AtomicBool,
        descend_packages: bool,
        id_base: u64,
        on_batch: impl FnMut(Vec<DiskUsageFact>),
        on_progress: impl FnMut(DiskUsageStats),
    ) -> Option<EnumerationError> {
        let mut next_raw = id_base.saturating_add(1);
        self.scan_disk_usage_with_identity(
            root,
            batch_size,
            cancel,
            descend_packages,
            move |_path| {
                let id = ferail_core::NodeId::from_raw(next_raw)
                    .expect("disk-usage scan-local ids are nonzero");
                next_raw = next_raw
                    .checked_add(1)
                    .expect("disk-usage scan-local identity space exhausted");
                id
            },
            on_batch,
            on_progress,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_disk_usage_with_identity(
        &self,
        root: &Path,
        batch_size: usize,
        cancel: &AtomicBool,
        descend_packages: bool,
        mut id_for_path: impl FnMut(&Path) -> ferail_core::NodeId,
        mut on_batch: impl FnMut(Vec<DiskUsageFact>),
        mut on_progress: impl FnMut(DiskUsageStats),
    ) -> Option<EnumerationError> {
        ferail_core::path_guard::assert_off_ui_thread("NativeFs::scan_disk_usage");
        // Canonicalize so firmlink twins and `..`-laden roots land on one
        // identity, but fall back to the path as given where the platform
        // can't (AROS's std stubs `canonicalize` as Unsupported; killing
        // the scan here made Disk Usage report "0 files" for every root).
        // A genuinely unreadable root is still caught by the read_dir
        // probe just below.
        let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        // Up-front check that the root is a directory we can read; mirrors
        // enumerate_streaming so callers see the same error class.
        match fs::read_dir(&canonical_root) {
            Ok(rd) => drop(rd),
            Err(e) => return Some(map_io_error(&e)),
        }

        let root_id = id_for_path(&canonical_root);
        let mut buffer: Vec<DiskUsageFact> = Vec::with_capacity(batch_size);
        let mut stats = DiskUsageStats::default();
        let mut last_progress = Instant::now();

        // Discover the root container itself so the tree has a kind/name.
        let root_name = canonical_root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned();
        let root_meta = fs::symlink_metadata(&canonical_root).ok();
        let root_mtime = root_meta.as_ref().and_then(|m| m.modified().ok());
        let root_dev = root_meta
            .as_ref()
            .and_then(file_identity)
            .map(|(dev, _, _)| dev);
        let icloud_root = icloud_root();
        let root_is_cloud = is_icloud_path_with_root(&canonical_root, icloud_root.as_deref());
        buffer.push(DiskUsageFact::NodeDiscovered {
            node: root_id,
            kind: NodeKind::Container,
            file_category: FileCategory::Other,
            mtime: root_mtime,
            name: root_name,
            is_cloud: root_is_cloud,
        });

        // Firmlink targets are the one sanctioned device crossing (see
        // the method docs). Read once per scan; empty off macOS.
        let firmlinks = firmlink_targets();
        // Directories already walked, keyed (dev, ino): insurance
        // against any remaining aliased-directory path (firmlink twins
        // are normally stopped by the boundary rule first).
        let mut seen_dirs: HashSet<(u64, u64)> = HashSet::new();
        // Hardlinked files already counted, keyed (dev, ino).
        let mut seen_links: HashSet<(u64, u64)> = HashSet::new();

        let workers = crate::directory_reader::recommended_recursive_workers(&canonical_root);
        crate::directory_reader::walk(
            DuDirectory {
                path: canonical_root,
                id: root_id,
                device: root_dev,
            },
            cancel,
            workers,
            |event| {
                use crate::directory_reader::DirectoryWalkEvent;
                let mut children = Vec::new();
                match event {
                    DirectoryWalkEvent::Started(directory) => {
                        buffer.push(DiskUsageFact::ContainerScanStarted {
                            container: directory.id,
                        });
                        stats.dirs_scanned = stats.dirs_scanned.saturating_add(1);
                    }
                    DirectoryWalkEvent::Batch(directory, entries) => {
                        for entry in entries {
                            let is_symlink = entry.is_symlink();
                            let is_dir = entry.is_dir() && !is_symlink;
                            let mtime = entry.mtime;
                            let identity = entry.identity;
                            let entry_size = entry.size;
                            let entry_allocated = entry.allocated;
                            let is_mount_point = entry.mount_point;
                            let child_path = entry.path;
                            let name = entry.name;
                            let child_id = id_for_path(&child_path);

                            let mut boundary_stub = is_mount_point;
                            if is_dir {
                                if let (Some((dev, ino, _)), Some(parent_dev)) =
                                    (identity, directory.device)
                                {
                                    let crosses = dev != parent_dev;
                                    let sanctioned =
                                        !crosses || firmlinks.iter().any(|f| f == &child_path);
                                    if !sanctioned || !seen_dirs.insert((dev, ino)) {
                                        boundary_stub = true;
                                    }
                                }
                            }

                            let mac_pkg = is_mac_package(&child_path);
                            let treat_as_leaf =
                                !is_dir || boundary_stub || (mac_pkg && !descend_packages);
                            let already_counted = if is_dir {
                                false
                            } else {
                                match identity {
                                    Some((dev, ino, nlink)) if nlink > 1 => {
                                        !seen_links.insert((dev, ino))
                                    }
                                    _ => false,
                                }
                            };

                            let (kind, file_category, size, allocated) = if treat_as_leaf {
                                let category = if boundary_stub {
                                    FileCategory::Other
                                } else if mac_pkg {
                                    FileCategory::Executable
                                } else {
                                    classify_path(&child_path)
                                };
                                let sizes = if is_symlink || boundary_stub || already_counted {
                                    (0, 0)
                                } else if mac_pkg && !descend_packages && is_dir {
                                    // The outer scan already owns the APFS I/O
                                    // pool. Keep this nested, opaque-package
                                    // rollup serial while still using native
                                    // bulk directory records; spawning another
                                    // pool here would oversubscribe the disk.
                                    let totals =
                                        recursive_totals_with_workers(&child_path, cancel, 1);
                                    (totals.apparent, totals.allocated)
                                } else {
                                    (entry_size, entry_allocated)
                                };
                                (NodeKind::File, category, sizes.0, sizes.1)
                            } else {
                                (NodeKind::Container, FileCategory::Other, 0, entry_allocated)
                            };

                            let child_is_cloud = root_is_cloud
                                || is_icloud_path_with_root(&child_path, icloud_root.as_deref());
                            buffer.push(DiskUsageFact::NodeDiscovered {
                                node: child_id,
                                kind,
                                file_category,
                                mtime,
                                name,
                                is_cloud: child_is_cloud,
                            });
                            buffer.push(DiskUsageFact::NodeLinked {
                                container: directory.id,
                                node: child_id,
                            });
                            if size > 0 {
                                buffer.push(DiskUsageFact::NodeSizeAdded {
                                    node: child_id,
                                    size_bytes: size,
                                });
                            }
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
                                children.push(DuDirectory {
                                    path: child_path,
                                    id: child_id,
                                    device: identity.map(|(dev, _, _)| dev),
                                });
                            }
                        }
                    }
                    DirectoryWalkEvent::Done(directory, error) => {
                        if let Some(error) = error {
                            stats.dirs_skipped = stats.dirs_skipped.saturating_add(1);
                            if error.kind() == std::io::ErrorKind::PermissionDenied {
                                stats.permission_denied_dirs =
                                    stats.permission_denied_dirs.saturating_add(1);
                            }
                        }
                        buffer.push(DiskUsageFact::ContainerScanCompleted {
                            container: directory.id,
                        });
                    }
                }

                if buffer.len() >= batch_size {
                    on_batch(std::mem::take(&mut buffer));
                    buffer.reserve(batch_size);
                }
                if last_progress.elapsed().as_millis() >= PROGRESS_THROTTLE_MS {
                    on_progress(stats);
                    last_progress = Instant::now();
                }
                children
            },
        );

        if !buffer.is_empty() {
            on_batch(buffer);
        }
        on_progress(stats);
        None
    }
}

/// Sum every regular file under `root`: logical bytes
/// (`metadata.len()`), the same semantic Finder's "Size" column uses.
/// Serves two callers: bundle rolled-up sizes inside the disk-usage
/// scan, and the file-list folder-size worker in `ferail-gpui`.
///
/// Iterative DFS, `symlink_metadata` only (no follow), absorbs
/// per-subdir read failures (returns whatever was summed before the
/// failure). `cancel` is checked between dirents; on cancel returns
/// the partial sum, which callers must treat as invalid (don't cache).
pub fn recursive_size(root: &Path, cancel: &AtomicBool) -> u64 {
    recursive_totals(root, cancel).apparent
}

/// Recursive rollup of a directory subtree in a single walk: byte
/// totals on both size axes **plus** item counts. `files` and `dirs`
/// are recursive (the whole subtree) and exclude `root` itself: every
/// regular file and every entered sub-directory found underneath.
/// Symlinks are excluded from every field (never followed); hardlinked
/// files count once; directories on a different device than `root`
/// (mount points) are neither entered nor counted, matching the size
/// axes exactly so a folder's counts and bytes always describe the same
/// set of entries. On cancel, returns whatever was tallied before the
/// stop: callers must treat a cancelled result as invalid (don't
/// cache, don't display).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubtreeTotals {
    /// Sum of `metadata.len()` over every counted file: Finder "Size".
    pub apparent: u64,
    /// Sum of allocated (block) sizes: the "on disk" axis.
    pub allocated: u64,
    /// Regular files underneath (recursive).
    pub files: u64,
    /// Entered sub-directories underneath (recursive).
    pub dirs: u64,
}

/// One-walk rollup behind [`recursive_size`] and the file list's folder-size
/// worker (which also wants item counts for the Description column). See
/// [`SubtreeTotals`] for the field semantics and the cancel contract.
pub fn recursive_totals(root: &Path, cancel: &AtomicBool) -> SubtreeTotals {
    let workers = crate::directory_reader::recommended_recursive_workers(root);
    recursive_totals_with_workers(root, cancel, workers)
}

fn recursive_totals_with_workers(
    root: &Path,
    cancel: &AtomicBool,
    workers: usize,
) -> SubtreeTotals {
    let mut t = SubtreeTotals::default();
    let root_dev = fs::symlink_metadata(root)
        .ok()
        .as_ref()
        .and_then(file_identity)
        .map(|(dev, _, _)| dev);
    let mut seen_links: HashSet<(u64, u64)> = HashSet::new();
    crate::directory_reader::walk(root.to_path_buf(), cancel, workers, |event| {
        use crate::directory_reader::DirectoryWalkEvent;

        let DirectoryWalkEvent::Batch(_directory, entries) = event else {
            return Vec::new();
        };
        let mut children = Vec::new();
        for entry in entries {
            if entry.is_symlink() {
                continue;
            }
            if entry.is_dir() {
                // Don't cross onto another filesystem (a mount point inside
                // the walk would roll a whole other volume into this folder's
                // number). Uncounted as well as unentered, so counts and bytes
                // describe the same subtree.
                if entry.mount_point
                    || matches!((entry.identity, root_dev), (Some((dev, _, _)), Some(root_dev)) if dev != root_dev)
                {
                    continue;
                }
                t.dirs = t.dirs.saturating_add(1);
                children.push(entry.path);
            } else {
                // Hardlinks count once (cp -al trees, Homebrew Cellar).
                if let Some((dev, ino, nlink)) = entry.identity {
                    if nlink > 1 && !seen_links.insert((dev, ino)) {
                        continue;
                    }
                }
                t.files = t.files.saturating_add(1);
                t.apparent = t.apparent.saturating_add(entry.size);
                t.allocated = t.allocated.saturating_add(entry.allocated);
            }
        }
        children
    });
    t
}

/// `(st_dev, st_ino, st_nlink)` on Unix; `None` where the identity
/// triple isn't exposed (Windows would need `BY_HANDLE_FILE_INFORMATION`
///, opening a handle per file, which the OneDrive-hydration rule
/// forbids on the scan path).
#[cfg(unix)]
fn file_identity(meta: &fs::Metadata) -> Option<(u64, u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino(), meta.nlink()))
}
#[cfg(not(unix))]
fn file_identity(_meta: &fs::Metadata) -> Option<(u64, u64, u64)> {
    None
}

/// The system→data crossings macOS sanctions for its merged volume
/// view, read from `/usr/share/firmlinks` (tab-separated, left column
/// is the system-volume path, e.g. `/Users`). Empty off macOS or if
/// the file is unreadable: the scan then simply refuses all device
/// crossings.
fn firmlink_targets() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        fs::read_to_string("/usr/share/firmlinks")
            .map(|s| {
                s.lines()
                    .filter_map(|line| line.split('\t').next())
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// Coarse iCloud-detection by path prefix: macOS stores all
/// ubiquity-managed files under `~/Library/Mobile Documents/`. Cheap
/// (a string starts_with), no NSURL call per file. Doesn't tell us
/// whether a given file is a downloaded copy vs a placeholder; the
/// renderer just paints a cloud glyph either way.
pub(crate) fn icloud_root() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library/Mobile Documents"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

pub(crate) fn is_icloud_path_with_root(path: &Path, root: Option<&Path>) -> bool {
    root.is_some_and(|root| path.starts_with(root))
}

/// macOS package detection by extension. Stays in sync with the
/// classify_extension list so categorization and descent agree.
pub(crate) fn is_mac_package(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    ["app", "bundle", "framework", "plugin", "kext", "xcodeproj"]
        .iter()
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
}

/// Filter helper for callers that want a single mtime as `SystemTime`
/// without taking a dep on `ferail-disk-usage` directly.
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
        let mut tree = ferail_disk_usage::DiskUsageTree::new(root_id);
        tree.apply_facts(&all);

        let layout = ferail_disk_usage::build_layout_node(&tree, root_id, 4);
        // 10 + 20 + 30 = 60 bytes total.
        assert_eq!(layout.size_bytes, 60);
        // Children: a.txt (10) + sub (50). Sorted descending → sub first.
        assert_eq!(layout.children.len(), 2);
        assert_eq!(layout.children[0].size_bytes, 50);
        assert_eq!(layout.children[0].kind, NodeKind::Container);
    }

    #[test]
    fn local_scan_ids_do_not_enter_process_path_map() {
        let tmp = fixture();
        let fs_native = NativeFs::new();
        let cancel = AtomicBool::new(false);
        let id_base = 1_u64 << 62;
        let mut all = Vec::new();
        let err = fs_native.scan_disk_usage_local(
            tmp.path(),
            DEFAULT_DU_BATCH,
            &cancel,
            false,
            id_base,
            |batch| all.extend(batch),
            |_| {},
        );
        assert!(err.is_none());

        let ids: Vec<_> = all
            .iter()
            .filter_map(|fact| match fact {
                DiskUsageFact::NodeDiscovered { node, .. } => Some(*node),
                _ => None,
            })
            .collect();
        assert_eq!(ids.first().map(|id| id.as_raw()), Some(id_base + 1));
        assert!(ids.iter().all(|id| fs_native.path_for(*id).is_none()));
    }

    #[test]
    fn recursive_size_sums_all_files() {
        let tmp = fixture();
        let cancel = AtomicBool::new(false);
        assert_eq!(recursive_size(tmp.path(), &cancel), 60);
    }

    #[test]
    fn recursive_totals_counts_files_and_dirs() {
        // fixture: a.txt + sub/{b.txt, c.png} → 3 files, 1 sub-dir,
        // 60 bytes. Counts are recursive and exclude the root itself.
        let tmp = fixture();
        let cancel = AtomicBool::new(false);
        let t = recursive_totals(tmp.path(), &cancel);
        assert_eq!(t.apparent, 60);
        assert_eq!(t.files, 3);
        assert_eq!(t.dirs, 1);
    }

    #[cfg(unix)]
    #[test]
    fn recursive_totals_hardlink_counts_once() {
        // A second name for a.txt must not inflate the file count or
        // the byte total, same dedup that protects the size axis.
        let tmp = fixture();
        let root = tmp.path();
        fs::hard_link(root.join("a.txt"), root.join("a-link.txt")).unwrap();
        let cancel = AtomicBool::new(false);
        let t = recursive_totals(root, &cancel);
        assert_eq!(t.apparent, 60);
        assert_eq!(t.files, 3);
        assert_eq!(t.dirs, 1);
    }

    #[test]
    fn recursive_totals_cancel_returns_early() {
        let tmp = fixture();
        let cancel = AtomicBool::new(true);
        let t = recursive_totals(tmp.path(), &cancel);
        assert_eq!(t, SubtreeTotals::default());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn hardlinked_files_count_once() {
        let tmp = fixture();
        let root = tmp.path();
        // a.txt (10 B) gains a second name: the bytes must not
        // count twice, in either the plain sum or the fact stream.
        fs::hard_link(root.join("a.txt"), root.join("a-link.txt")).unwrap();
        let cancel = AtomicBool::new(false);
        // The small synchronous helper still uses portable `std::fs`
        // metadata on Windows. The production scan below uses the native
        // batched reader, whose file IDs make hard-link accounting exact.
        #[cfg(unix)]
        assert_eq!(recursive_size(root, &cancel), 60);

        let fs_native = NativeFs::new();
        let mut all = Vec::new();
        let err = fs_native.scan_disk_usage(
            root,
            DEFAULT_DU_BATCH,
            &cancel,
            false,
            |batch| all.extend(batch),
            |_| {},
        );
        assert!(err.is_none());
        let canonical = fs::canonicalize(root).unwrap();
        let root_id = fs_native.id_for_path(&canonical);
        let mut tree = ferail_disk_usage::DiskUsageTree::new(root_id);
        tree.apply_facts(&all);
        let layout = ferail_disk_usage::build_layout_node(&tree, root_id, 4);
        assert_eq!(layout.size_bytes, 60);
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
            let path = base.join(format!("ferail-{prefix}-{pid}-{nanos}-{n}"));
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
