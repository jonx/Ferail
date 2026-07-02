//! Duplicate-finder funnel ([docs/features/DUPLICATES.md]).
//!
//! The progressive funnel every serious dedup tool converges on, because
//! it minimizes I/O:
//!
//! 1. **Walk + group by size** — a unique size cannot be a duplicate, so
//!    it is dropped with zero hashing.
//! 2. **Partial hash** (first 64 KB, xxh3) on size-collision groups only.
//! 3. **Full hash** (BLAKE3) on partial-hash-collision groups only — or,
//!    with `paranoid`, a byte-for-byte confirm that removes any
//!    hash-collision doubt.
//! 4. **Group by full hash** and emit.
//!
//! Duplicate finding is I/O-bound, not CPU-bound: hashing runs at
//! multi-GB/s, the disk is the bottleneck. The two levers that matter are
//! therefore (a) reading as little as possible — the funnel — and (b) not
//! re-reading across runs — the optional [`DupeHashCache`], which the
//! GPUI layer backs with the `files` table so a rescan skips full hashing
//! entirely.
//!
//! Mac-safe, matching the disk-usage walker: dataless iCloud placeholders
//! are skipped (never downloaded to hash), packages are opaque by
//! default, symlinks are never followed, per-dir read errors are
//! absorbed. Hard links (and, later, APFS clones) are flagged because
//! they share storage — they are duplicate *file IDs*, not duplicate
//! *bytes*, and reclaim no space if deleted.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use feraille_core::{EnumerationError, NodeId};

use crate::disk_usage_scanner::is_mac_package;
use crate::{map_io_error, NativeFs};

/// Bytes hashed for the partial-hash filter stage.
pub const PARTIAL_HASH_BYTES: usize = 64 * 1024;
/// Default fact-batch size.
pub const DEFAULT_DUPE_BATCH: usize = 64;
const PROGRESS_THROTTLE_MS: u128 = 250;

/// Scan options.
#[derive(Clone, Debug)]
pub struct DupeOpts {
    /// Verify byte-for-byte within each full-hash group, splitting off any
    /// member that doesn't actually match. Removes hash-collision risk at
    /// the cost of re-reading confirmed groups.
    pub paranoid: bool,
    /// Hash undownloaded iCloud placeholders too (forces a download).
    /// Off by default — the prime directive forbids surprise downloads.
    pub scan_cloud: bool,
    /// Descend into macOS packages (`*.app`, `*.bundle`, …) and compare
    /// their inner files. Off by default: packages are opaque leaves and
    /// are not themselves compared (whole-bundle comparison is future).
    pub follow_packages: bool,
    /// Ignore files smaller than this. 0-byte files are always ignored.
    pub min_size: u64,
}

impl Default for DupeOpts {
    fn default() -> Self {
        Self {
            paranoid: false,
            scan_cloud: false,
            follow_packages: false,
            min_size: 1,
        }
    }
}

/// One member of a duplicate group.
#[derive(Clone, Debug)]
pub struct DupeMember {
    pub node: NodeId,
    pub path: PathBuf,
    /// Last-modified time (Unix seconds), so the UI can offer
    /// "keep newest" without re-stat-ing each row.
    pub mtime_unix: i64,
    /// `(dev, inode)` identity. Members sharing this are hard links to the
    /// same bytes — one occupant of disk, several names.
    pub file_id: Option<(u64, u64)>,
    /// This member's `(dev, inode)` already appeared earlier in the
    /// group: it is a hard link, so deleting it reclaims nothing.
    pub is_hardlink: bool,
    /// This member is a distinct inode that nonetheless shares physical
    /// storage with an earlier member via an APFS clone (`clonefile`):
    /// deleting it reclaims nothing either. Always `false` off macOS.
    pub is_clone: bool,
}

impl DupeMember {
    /// True for a member that occupies no storage of its own — a hard
    /// link or a clone of an earlier member. The reclaimable total
    /// counts only the others.
    pub fn shares_storage(&self) -> bool {
        self.is_hardlink || self.is_clone
    }
}

/// Streamed result of the funnel.
#[derive(Clone, Debug)]
pub enum DupeFact {
    /// A confirmed group of ≥2 byte-identical files.
    Group {
        /// BLAKE3 hex of the content (empty in `paranoid` mode, where
        /// grouping is by byte-equality rather than a digest).
        full_hash: String,
        /// Logical bytes of each member.
        bytes_each: u64,
        members: Vec<DupeMember>,
        /// Distinct on-disk occupants: the number of unique `file_id`s.
        /// `members.len() - distinct_occupants` names are hard links that
        /// reclaim nothing if removed.
        distinct_occupants: usize,
    },
}

/// Running totals for the status bar.
#[derive(Clone, Copy, Debug, Default)]
pub struct DupeStats {
    pub files_scanned: u64,
    pub bytes_hashed: u64,
    pub groups_found: u64,
}

/// Persistent hash cache, keyed on `(path, size, mtime)`. The GPUI layer
/// implements this over the `files` table so a second scan of a tree
/// reuses prior full hashes and skips the expensive read. `None` (no
/// cache) is always correct, just slower on rescans.
pub trait DupeHashCache {
    /// Cached BLAKE3 hex for this exact `(path, size, mtime)`, if any.
    fn get_full(&self, path: &Path, size: u64, mtime_unix: i64) -> Option<String>;
    /// Record a freshly computed BLAKE3 hex.
    fn put_full(&self, path: &Path, size: u64, mtime_unix: i64, hash: &str);
}

/// A candidate file gathered during the walk.
struct Candidate {
    node: NodeId,
    path: PathBuf,
    size: u64,
    mtime_unix: i64,
    file_id: Option<(u64, u64)>,
}

impl NativeFs {
    /// Run the duplicate-finder funnel over `root`. Emits a [`DupeFact`]
    /// per confirmed group through `on_batch`; reports throttled progress
    /// through `on_progress`. `cancel` is honored at every stage.
    ///
    /// `cache`, when supplied, is consulted before full-hashing and
    /// written through on miss.
    // The duplicate-finder funnel genuinely needs each of these inputs.
    #[allow(clippy::too_many_arguments)]
    pub fn find_duplicates(
        &self,
        root: &Path,
        opts: &DupeOpts,
        cache: Option<&dyn DupeHashCache>,
        batch_size: usize,
        cancel: &AtomicBool,
        mut on_batch: impl FnMut(Vec<DupeFact>),
        mut on_progress: impl FnMut(DupeStats),
    ) -> Option<EnumerationError> {
        let canonical_root = match fs::canonicalize(root) {
            Ok(p) => p,
            Err(e) => return Some(map_io_error(&e)),
        };
        match fs::read_dir(&canonical_root) {
            Ok(rd) => drop(rd),
            Err(e) => return Some(map_io_error(&e)),
        }

        let mut stats = DupeStats::default();
        let mut last_progress = Instant::now();

        // Stage 1: walk + bucket by size.
        let mut by_size: HashMap<u64, Vec<Candidate>> = HashMap::new();
        let mut stack: Vec<PathBuf> = vec![canonical_root];
        while let Some(dir) = stack.pop() {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let read_dir = match fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            for dirent in read_dir.flatten() {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                let path = dirent.path();
                let meta = match fs::symlink_metadata(&path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let ft = meta.file_type();
                if ft.is_symlink() {
                    continue;
                }
                if ft.is_dir() {
                    let pkg = is_mac_package(&path);
                    if pkg && !opts.follow_packages {
                        continue; // opaque leaf, not compared
                    }
                    stack.push(path);
                    continue;
                }
                // Regular file.
                if !opts.scan_cloud && is_dataless(&meta) {
                    continue;
                }
                let size = meta.len();
                if size == 0 || size < opts.min_size {
                    continue;
                }
                stats.files_scanned = stats.files_scanned.saturating_add(1);
                let node = self.id_for_path(&path);
                by_size.entry(size).or_default().push(Candidate {
                    node,
                    path,
                    size,
                    mtime_unix: mtime_unix(&meta),
                    file_id: file_id(&meta),
                });
            }
            if last_progress.elapsed().as_millis() >= PROGRESS_THROTTLE_MS {
                on_progress(stats);
                last_progress = Instant::now();
            }
        }

        // Stage 2 + 3: within each size collision, partial-hash, then
        // full-hash the partial collisions, then group + emit.
        let mut buffer: Vec<DupeFact> = Vec::with_capacity(batch_size);
        for (size, cands) in by_size {
            if cands.len() < 2 {
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                if !buffer.is_empty() {
                    on_batch(std::mem::take(&mut buffer));
                }
                return None;
            }

            // Partial-hash filter.
            let mut by_partial: HashMap<u64, Vec<Candidate>> = HashMap::new();
            for c in cands {
                if cancel.load(Ordering::Relaxed) {
                    if !buffer.is_empty() {
                        on_batch(std::mem::take(&mut buffer));
                    }
                    return None;
                }
                let Some(ph) = partial_hash(&c.path) else {
                    continue;
                };
                stats.bytes_hashed = stats
                    .bytes_hashed
                    .saturating_add(size.min(PARTIAL_HASH_BYTES as u64));
                by_partial.entry(ph).or_default().push(c);
            }

            for (_ph, group) in by_partial {
                if group.len() < 2 {
                    continue;
                }
                if cancel.load(Ordering::Relaxed) {
                    if !buffer.is_empty() {
                        on_batch(std::mem::take(&mut buffer));
                    }
                    return None;
                }

                // Full-hash (cache-aware) the partial collision.
                let mut by_full: HashMap<String, Vec<Candidate>> = HashMap::new();
                for c in group {
                    let full = match cache.and_then(|cc| cc.get_full(&c.path, c.size, c.mtime_unix))
                    {
                        Some(h) => h,
                        None => {
                            let Some(h) = full_hash(&c.path) else {
                                continue;
                            };
                            stats.bytes_hashed = stats.bytes_hashed.saturating_add(size);
                            if let Some(cc) = cache {
                                cc.put_full(&c.path, c.size, c.mtime_unix, &h);
                            }
                            h
                        }
                    };
                    by_full.entry(full).or_default().push(c);
                }

                for (full_hash_hex, members) in by_full {
                    if members.len() < 2 {
                        continue;
                    }
                    let groups = if opts.paranoid {
                        byte_verify_split(members)
                    } else {
                        vec![members]
                    };
                    for grp in groups {
                        if grp.len() < 2 {
                            continue;
                        }
                        let members = build_members(grp);
                        // Distinct on-disk occupants = members that own
                        // their storage (neither a hard link nor a clone
                        // of an earlier member). Reclaim counts the rest.
                        let distinct_occupants =
                            members.iter().filter(|m| !m.shares_storage()).count();
                        stats.groups_found = stats.groups_found.saturating_add(1);
                        buffer.push(DupeFact::Group {
                            full_hash: if opts.paranoid {
                                String::new()
                            } else {
                                full_hash_hex.clone()
                            },
                            bytes_each: size,
                            distinct_occupants,
                            members,
                        });
                        if buffer.len() >= batch_size {
                            on_batch(std::mem::take(&mut buffer));
                            buffer.reserve(batch_size);
                        }
                    }
                }
            }
        }

        if !buffer.is_empty() {
            on_batch(buffer);
        }
        on_progress(stats);
        None
    }
}

/// Turn confirmed candidates into [`DupeMember`]s, flagging storage
/// sharing. A member is a **hard link** when its `(dev, inode)` already
/// appeared earlier in the group (several names, one inode). It is a
/// **clone** when it is a fresh inode that nonetheless shares physical
/// blocks with an earlier member (APFS `clonefile`); clone detection is
/// macOS-only and runs in [`mark_clones`]. Members with no `file_id`
/// (non-Unix) are each their own occupant.
fn build_members(group: Vec<Candidate>) -> Vec<DupeMember> {
    let mut seen_ids: Vec<(u64, u64)> = Vec::new();
    let mut members: Vec<DupeMember> = group
        .into_iter()
        .map(|c| {
            let is_hardlink = match c.file_id {
                Some(id) => {
                    let dup = seen_ids.contains(&id);
                    if !dup {
                        seen_ids.push(id);
                    }
                    dup
                }
                None => false,
            };
            DupeMember {
                node: c.node,
                path: c.path,
                mtime_unix: c.mtime_unix,
                file_id: c.file_id,
                is_hardlink,
                is_clone: false,
            }
        })
        .collect();
    mark_clones(&mut members);
    members
}

/// Flag members that share physical storage with an earlier member via
/// an APFS clone. Off macOS (and for the non-macOS test build) this is a
/// no-op: clones leave no portable signal, so nothing is reclaimed
/// incorrectly. The macOS body lives in [`clone_detect`].
fn mark_clones(members: &mut [DupeMember]) {
    #[cfg(target_os = "macos")]
    clone_detect::mark(members);
    #[cfg(not(target_os = "macos"))]
    let _ = members;
}

/// Reclaim a duplicate's bytes *without deleting its name*: remove the
/// `victim` and recreate it as an APFS clone of `keeper`. The clone
/// shares the keeper's blocks, so the victim's independent copy is freed
/// while the file stays present and byte-identical — the zero-copy
/// counterpart to trashing. macOS/APFS only.
///
/// **Destructive and only safe within a confirmed duplicate group.** The
/// victim's original inode is unlinked; if `clonefile` then fails the
/// name is momentarily gone, but because keeper and victim were
/// byte-identical (same full-hash group) the content still lives in the
/// keeper and the caller can restore by copying it back. Callers must
/// confirm with the user and should only offer this on an APFS volume.
#[cfg(target_os = "macos")]
pub fn clone_dedup(keeper: &Path, victim: &Path) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let keeper_c = CString::new(keeper.as_os_str().as_bytes())
        .map_err(|_| "clone_dedup: keeper path contains NUL".to_string())?;
    let victim_c = CString::new(victim.as_os_str().as_bytes())
        .map_err(|_| "clone_dedup: victim path contains NUL".to_string())?;
    // clonefile refuses to overwrite an existing destination, so drop the
    // victim first. (It is byte-identical to the keeper we're about to
    // clone, so no unique data is lost.)
    std::fs::remove_file(victim).map_err(|e| format!("remove {}: {e}", victim.display()))?;
    let rc = unsafe { libc::clonefile(keeper_c.as_ptr(), victim_c.as_ptr(), 0) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!(
            "clonefile {} \u{2192} {}: {err}",
            keeper.display(),
            victim.display()
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn clone_dedup(_keeper: &Path, _victim: &Path) -> Result<(), String> {
    Err("clone dedup is available only on macOS / APFS".into())
}

/// Paranoid mode: split a same-hash group into byte-equal clusters,
/// guarding against the (astronomically unlikely) hash collision.
/// Streams each comparison in fixed 256 KB chunks against each
/// cluster's representative — `fs::read`-ing whole members held one
/// full copy per cluster simultaneously, which OOM'd on groups of
/// multi-GB videos.
fn byte_verify_split(members: Vec<Candidate>) -> Vec<Vec<Candidate>> {
    let mut clusters: Vec<Vec<Candidate>> = Vec::new();
    'outer: for c in members {
        for group in clusters.iter_mut() {
            let rep = &group[0];
            if matches!(files_equal_streaming(&rep.path, &c.path), Ok(true)) {
                group.push(c);
                continue 'outer;
            }
        }
        clusters.push(vec![c]);
    }
    clusters
}

/// Byte-equality of two files via two 256 KB buffers — constant
/// memory regardless of file size. `Err` on any read failure (the
/// caller treats the pair as unequal).
fn files_equal_streaming(a: &Path, b: &Path) -> std::io::Result<bool> {
    const CHUNK: usize = 256 * 1024;
    let mut fa = fs::File::open(a)?;
    let mut fb = fs::File::open(b)?;
    let mut ba = vec![0u8; CHUNK];
    let mut bb = vec![0u8; CHUNK];
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

/// Fill `buf` as far as the stream allows; returns bytes read (0 at
/// EOF). Plain `read` may return short counts mid-file, which would
/// desync the two streams' chunk boundaries.
fn read_full(file: &mut fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// xxh3 of the first [`PARTIAL_HASH_BYTES`] of the file. `None` on read
/// error.
fn partial_hash(path: &Path) -> Option<u64> {
    let mut file = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; PARTIAL_HASH_BYTES];
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => return None,
        }
    }
    Some(xxhash_rust::xxh3::xxh3_64(&buf[..filled]))
}

/// BLAKE3 hex of the entire file, read in chunks so we never hold a huge
/// file fully in memory. `None` on read error.
fn full_hash(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
            }
            Err(_) => return None,
        }
    }
    Some(hasher.finalize().to_hex().to_string())
}

fn mtime_unix(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `(dev, inode)` so hard links to one inode collapse to one occupant.
#[cfg(unix)]
fn file_id(meta: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(_meta: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// Undownloaded cloud placeholder (APFS `SF_DATALESS`) — reading it would
/// trigger a network download.
#[cfg(target_os = "macos")]
fn is_dataless(meta: &fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    const SF_DATALESS: u32 = 0x4000_0000;
    (meta.st_flags() & SF_DATALESS) != 0
}

#[cfg(not(target_os = "macos"))]
fn is_dataless(_meta: &fs::Metadata) -> bool {
    false
}

/// APFS clone detection (macOS only).
///
/// Members of a confirmed full-hash group already have identical content;
/// what distinguishes a *clone* (`clonefile`) from an independent copy is
/// that the clone shares the same physical blocks. We probe the device
/// offset of each member's first logical byte with `fcntl(F_LOG2PHYS_EXT)`
/// — two storage-owning members on the same device that map block 0 to
/// the same physical offset are clones of one another, so all but the
/// first reclaim nothing and must be excluded from the reclaim total,
/// exactly like hard links.
#[cfg(target_os = "macos")]
mod clone_detect {
    use std::collections::hash_map::Entry;
    use std::collections::HashMap;
    use std::fs::File;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    use super::DupeMember;

    /// Flag members that share physical storage with an earlier,
    /// storage-owning member via an APFS clone.
    pub(super) fn mark(members: &mut [DupeMember]) {
        // (device, physical byte offset of logical block 0) → first
        // member that claimed it. A later member hitting the same key is
        // a clone of that one.
        let mut seen: HashMap<(u64, i64), usize> = HashMap::new();
        for (i, member) in members.iter_mut().enumerate() {
            // Hard links are already collapsed by inode; their physical
            // offset would (correctly) collide with their inode-sibling
            // but we don't want to double-count them as clones.
            if member.is_hardlink {
                continue;
            }
            let Some((dev, _)) = member.file_id else {
                continue;
            };
            let Some(phys) = first_block_phys(&member.path) else {
                continue;
            };
            match seen.entry((dev, phys)) {
                Entry::Occupied(_) => member.is_clone = true,
                Entry::Vacant(v) => {
                    v.insert(i);
                }
            }
        }
    }

    /// Physical device byte offset backing logical offset 0, or `None`
    /// when it can't be determined (open/fcntl failure, or a sparse /
    /// unmapped / compressed extent that reports no real block — treated
    /// conservatively as "not provably a clone").
    fn first_block_phys(path: &Path) -> Option<i64> {
        let file = File::open(path).ok()?;
        // F_LOG2PHYS_EXT reads the query range from the struct: map one
        // byte at logical offset 0. On success `l2p_devoffset` holds the
        // physical byte offset on the backing device.
        let mut l2p = libc::log2phys {
            l2p_flags: 0,
            l2p_contigbytes: 1,
            l2p_devoffset: 0,
        };
        let rc = unsafe {
            libc::fcntl(
                file.as_raw_fd(),
                libc::F_LOG2PHYS_EXT,
                &mut l2p as *mut libc::log2phys,
            )
        };
        if rc != 0 || l2p.l2p_devoffset <= 0 {
            return None;
        }
        Some(l2p.l2p_devoffset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::AtomicU32;

    struct Fixture {
        root: PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    impl Fixture {
        fn path(&self) -> &Path {
            &self.root
        }
        fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let p = self.root.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::File::create(&p).unwrap().write_all(bytes).unwrap();
            p
        }
    }

    fn fixture() -> Fixture {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("feraille-dupefix-{}-{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn run(root: &Path, opts: DupeOpts) -> Vec<DupeFact> {
        let fs = NativeFs::new();
        let cancel = AtomicBool::new(false);
        let mut facts = Vec::new();
        fs.find_duplicates(root, &opts, None, 8, &cancel, |b| facts.extend(b), |_| {});
        facts
    }

    #[test]
    fn finds_identical_content_across_dirs() {
        let fx = fixture();
        fx.write("a.bin", b"hello world duplicate content here");
        fx.write("sub/b.bin", b"hello world duplicate content here");
        fx.write("unique.bin", b"i am a completely different file ok");
        let facts = run(fx.path(), DupeOpts::default());
        assert_eq!(facts.len(), 1, "exactly one duplicate group");
        let DupeFact::Group {
            members,
            distinct_occupants,
            ..
        } = &facts[0];
        assert_eq!(members.len(), 2);
        assert_eq!(*distinct_occupants, 2, "two separate inodes");
    }

    #[test]
    fn same_size_different_content_is_not_a_dupe() {
        let fx = fixture();
        // Same length, different bytes — survives size grouping, killed by
        // hashing.
        fx.write("a.bin", b"AAAAAAAAAAAAAAAA");
        fx.write("b.bin", b"BBBBBBBBBBBBBBBB");
        let facts = run(fx.path(), DupeOpts::default());
        assert!(facts.is_empty());
    }

    #[test]
    fn unique_sizes_are_dropped_without_hashing() {
        let fx = fixture();
        fx.write("a.bin", b"short");
        fx.write("b.bin", b"a much longer file body");
        let facts = run(fx.path(), DupeOpts::default());
        assert!(facts.is_empty());
    }

    #[test]
    fn hard_links_collapse_to_one_occupant() {
        let fx = fixture();
        let a = fx.write("a.bin", b"linked identical bytes for the test");
        let b = fx.root.join("b.bin");
        #[cfg(unix)]
        std::fs::hard_link(&a, &b).unwrap();
        #[cfg(not(unix))]
        {
            let _ = &a;
            fs::write(&b, b"linked identical bytes for the test").unwrap();
        }
        let facts = run(fx.path(), DupeOpts::default());
        assert_eq!(facts.len(), 1);
        let DupeFact::Group {
            members,
            distinct_occupants,
            ..
        } = &facts[0];
        assert_eq!(members.len(), 2, "both names reported");
        #[cfg(unix)]
        assert_eq!(*distinct_occupants, 1, "hard links share one inode");
    }

    #[test]
    fn cache_supplies_full_hash_and_skips_read() {
        // A cache that claims every file hashes to the same value should
        // force a single group without the worker reading content.
        struct AllSame;
        impl DupeHashCache for AllSame {
            fn get_full(&self, _p: &Path, _s: u64, _m: i64) -> Option<String> {
                Some("CACHED".to_string())
            }
            fn put_full(&self, _p: &Path, _s: u64, _m: i64, _h: &str) {}
        }
        let fx = fixture();
        // Same size so they reach the full-hash stage; partial hashes
        // match because content is identical up to the cache override.
        fx.write("a.bin", b"1234567890123456");
        fx.write("b.bin", b"1234567890123456");
        let fs = NativeFs::new();
        let cancel = AtomicBool::new(false);
        let cache = AllSame;
        let mut facts = Vec::new();
        fs.find_duplicates(
            fx.path(),
            &DupeOpts::default(),
            Some(&cache),
            8,
            &cancel,
            |b| facts.extend(b),
            |_| {},
        );
        assert_eq!(facts.len(), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn apfs_clone_is_flagged_and_excluded_from_reclaim() {
        let fx = fixture();
        let a = fx.write("a.bin", b"clone me clone me clone me clone");
        let b = fx.write("b.bin", b"clone me clone me clone me clone");
        // Turn b into an APFS clone of a so they share physical storage.
        // Skips gracefully if the temp dir isn't on an APFS volume.
        if clone_dedup(&a, &b).is_err() {
            return;
        }
        let facts = run(fx.path(), DupeOpts::default());
        assert_eq!(facts.len(), 1);
        let DupeFact::Group {
            members,
            distinct_occupants,
            ..
        } = &facts[0];
        assert_eq!(members.len(), 2, "both names still reported");
        assert_eq!(
            members.iter().filter(|m| m.is_clone).count(),
            1,
            "exactly one member detected as a clone"
        );
        assert_eq!(
            *distinct_occupants, 1,
            "clone shares storage → one occupant"
        );
    }

    #[test]
    fn paranoid_still_groups_true_duplicates() {
        let fx = fixture();
        fx.write("a.bin", b"paranoid mode identical bytes content");
        fx.write("b.bin", b"paranoid mode identical bytes content");
        let facts = run(
            fx.path(),
            DupeOpts {
                paranoid: true,
                ..DupeOpts::default()
            },
        );
        assert_eq!(facts.len(), 1);
        let DupeFact::Group {
            members, full_hash, ..
        } = &facts[0];
        assert_eq!(members.len(), 2);
        assert!(full_hash.is_empty(), "paranoid groups carry no digest");
    }

    #[test]
    fn cancel_before_emit_yields_nothing() {
        let fx = fixture();
        fx.write("a.bin", b"identical identical identical xx");
        fx.write("b.bin", b"identical identical identical xx");
        let fs = NativeFs::new();
        let cancel = AtomicBool::new(true);
        let mut facts = Vec::new();
        let err = fs.find_duplicates(
            fx.path(),
            &DupeOpts::default(),
            None,
            8,
            &cancel,
            |b| facts.extend(b),
            |_| {},
        );
        assert!(err.is_none());
        assert!(facts.is_empty());
    }
}
