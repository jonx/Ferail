//! Copy / move engine for file operations (docs/features/FILE_OPS.md).
//!
//! Pure, synchronous, worker-thread functions — the GPUI side owns
//! scheduling, dialogs, and undo. Contract mirrors `recursive_size`:
//! cooperative cancellation via `&AtomicBool`, progress via callback,
//! partial results reported honestly in the outcome.
//!
//! Never touches UI, pasteboard, SQLite, or AppKit.

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Streaming copy buffer. Large enough that syscall overhead is
/// negligible, small enough that progress ticks and cancellation
/// lands quickly even on slow volumes.
const COPY_CHUNK: usize = 8 * 1024 * 1024;

/// Everything `plan_transfer` learns up front so the UI can size the
/// progress bar and raise the collision dialog before any bytes move.
#[derive(Debug)]
pub struct OpPlan {
    /// Top-level items being copied/moved (as given, after validation).
    pub sources: Vec<PathBuf>,
    pub dest_dir: PathBuf,
    /// Sum of file sizes across all sources (directories walk free).
    pub total_bytes: u64,
    /// Files + directories, for outcome bookkeeping.
    pub total_items: u64,
    /// Destination paths (`dest_dir/<name>`) that already exist.
    pub conflicts: Vec<PathBuf>,
}

/// One policy for the whole batch (per-item resolution is a later
/// iter — see the design doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollisionPolicy {
    /// Remove the existing destination just before copying that item.
    Replace,
    /// Pick a fresh name (`"name 2"`, `"name 3"`, …).
    KeepBoth,
    /// Leave the existing destination alone; don't transfer that item.
    Skip,
}

/// What actually happened. `created` pairs each transferred top-level
/// source with the destination it produced (post collision-renaming),
/// which is exactly what undo needs.
#[derive(Default)]
pub struct OpOutcome {
    pub created: Vec<(PathBuf, PathBuf)>,
    pub skipped: u64,
    pub replaced: u64,
    pub cancelled: bool,
}

/// Naming scheme for collision-free names.
#[derive(Clone, Copy)]
pub enum NameScheme {
    /// Finder Duplicate flavor: `name copy`, `name copy 2`, …
    Copy,
    /// Finder paste-collision flavor: `name 2`, `name 3`, …
    Numbered,
}

/// First non-existing `dest_dir/<variant of name>` under `scheme`.
/// Extension is preserved (`a.txt` → `a 2.txt`). `None` after 9999
/// candidates (pathological; caller treats as an error).
pub fn pick_available_name(dest_dir: &Path, name: &OsStr, scheme: NameScheme) -> Option<PathBuf> {
    let as_path = Path::new(name);
    let stem = as_path.file_stem().unwrap_or(name).to_string_lossy();
    let ext = as_path.extension();
    for n in 1..=9999u32 {
        let candidate_stem = match scheme {
            NameScheme::Copy if n == 1 => format!("{stem} copy"),
            NameScheme::Copy => format!("{stem} copy {n}"),
            // Numbered starts at 2: "name" exists, next is "name 2".
            NameScheme::Numbered => format!("{stem} {}", n + 1),
        };
        let mut candidate = dest_dir.join(&candidate_stem);
        if let Some(e) = ext {
            candidate.set_extension(e);
        }
        if !candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Whether two paths live on the same filesystem (move can use the
/// `rename` fast path). Resolves against the nearest existing
/// ancestor so a not-yet-created destination still answers.
/// [win-parity: compare `GetVolumePathNameW` roots]
pub fn same_volume(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fn dev_of(p: &Path) -> Option<u64> {
            let mut cur = Some(p);
            while let Some(c) = cur {
                if let Ok(m) = fs::symlink_metadata(c) {
                    return Some(m.dev());
                }
                cur = c.parent();
            }
            None
        }
        match (dev_of(a), dev_of(b)) {
            (Some(da), Some(db)) => da == db,
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        // Conservative: forces the copy+delete path, which is always
        // correct, just slower. Real volume identity lands with the
        // Windows parity pass.
        let _ = (a, b);
        false
    }
}

/// Walk the sources, total the work, and detect top-level collisions.
/// Errors when a source is missing, the destination isn't a
/// directory, or the destination sits *inside* a source (copying a
/// folder into its own subtree never terminates).
pub fn plan_transfer(
    sources: &[PathBuf],
    dest_dir: &Path,
    cancel: &AtomicBool,
) -> Result<OpPlan, String> {
    if !dest_dir.is_dir() {
        return Err(format!("{} is not a folder", dest_dir.display()));
    }
    let mut total_bytes = 0u64;
    let mut total_items = 0u64;
    let mut conflicts = Vec::new();
    for src in sources {
        let meta = fs::symlink_metadata(src)
            .map_err(|e| format!("{}: {e}", src.display()))?;
        if dest_dir.starts_with(src) {
            return Err(format!(
                "can't copy \u{201c}{}\u{201d} into itself",
                src.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        let Some(name) = src.file_name() else {
            return Err(format!("{}: no file name", src.display()));
        };
        if dest_dir.join(name).exists() {
            conflicts.push(dest_dir.join(name));
        }
        if meta.is_dir() && !meta.is_symlink() {
            // Iterative walk, same shape as recursive_size.
            let mut stack = vec![src.clone()];
            while let Some(dir) = stack.pop() {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                total_items += 1;
                let Ok(rd) = fs::read_dir(&dir) else { continue };
                for dirent in rd.flatten() {
                    let p = dirent.path();
                    let Ok(m) = fs::symlink_metadata(&p) else { continue };
                    if m.is_dir() && !m.is_symlink() {
                        stack.push(p);
                    } else {
                        total_items += 1;
                        total_bytes = total_bytes.saturating_add(m.len());
                    }
                }
            }
        } else {
            total_items += 1;
            total_bytes = total_bytes.saturating_add(meta.len());
        }
    }
    Ok(OpPlan {
        sources: sources.to_vec(),
        dest_dir: dest_dir.to_path_buf(),
        total_bytes,
        total_items,
        conflicts,
    })
}

/// Resolve where a top-level item lands under `policy`. `None` =
/// skip this item. Replace deletes the existing destination here —
/// immediately before its copy starts, never earlier.
fn resolve_dest(
    src: &Path,
    dest_dir: &Path,
    policy: CollisionPolicy,
    outcome: &mut OpOutcome,
) -> Result<Option<PathBuf>, String> {
    let name = src
        .file_name()
        .ok_or_else(|| format!("{}: no file name", src.display()))?;
    let plain = dest_dir.join(name);
    if !plain.exists() && !plain.is_symlink() {
        return Ok(Some(plain));
    }
    match policy {
        CollisionPolicy::Skip => {
            outcome.skipped += 1;
            Ok(None)
        }
        CollisionPolicy::Replace => {
            let meta = fs::symlink_metadata(&plain)
                .map_err(|e| format!("{}: {e}", plain.display()))?;
            let removed = if meta.is_dir() && !meta.is_symlink() {
                fs::remove_dir_all(&plain)
            } else {
                fs::remove_file(&plain)
            };
            removed.map_err(|e| format!("replace {}: {e}", plain.display()))?;
            outcome.replaced += 1;
            Ok(Some(plain))
        }
        CollisionPolicy::KeepBoth => pick_available_name(dest_dir, name, NameScheme::Numbered)
            .map(Some)
            .ok_or_else(|| format!("no free name for {} in {}", name.to_string_lossy(), dest_dir.display())),
    }
}

/// Copy one file in chunks, ticking `done` bytes through `progress`
/// and honoring `cancel` between chunks. A cancelled partial
/// destination is removed so nothing half-written survives.
fn copy_file_chunked(
    src: &Path,
    dst: &Path,
    done: &mut u64,
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<bool, String> {
    let mut reader =
        fs::File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let mut writer =
        fs::File::create(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let mut buf = vec![0u8; COPY_CHUNK];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", src.display()))?;
        if n == 0 {
            break;
        }
        // Cancel only while bytes remain — a file whose final chunk
        // already landed is complete, not partial, and must survive.
        if cancel.load(Ordering::Relaxed) {
            drop(writer);
            let _ = fs::remove_file(dst);
            return Ok(false);
        }
        writer
            .write_all(&buf[..n])
            .map_err(|e| format!("{}: {e}", dst.display()))?;
        *done = done.saturating_add(n as u64);
        progress(*done, total);
    }
    if let Ok(meta) = fs::metadata(src) {
        // Best-effort permission carry-over; not fatal.
        let _ = fs::set_permissions(dst, meta.permissions());
    }
    Ok(true)
}

/// Copy one top-level item (file, dir tree, or symlink). Returns
/// `false` on cancellation; the partially-copied current file is
/// removed but already-completed files inside the item stay (the
/// caller reports the partial state).
fn copy_item(
    src: &Path,
    dst: &Path,
    done: &mut u64,
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<bool, String> {
    let meta = fs::symlink_metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if meta.is_symlink() {
        return recreate_symlink(src, dst).map(|()| true);
    }
    if !meta.is_dir() {
        return copy_file_chunked(src, dst, done, total, progress, cancel);
    }
    // Directory: depth-first with explicit stack of (src, dst) pairs.
    fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let mut stack: Vec<(PathBuf, PathBuf)> = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((sdir, ddir)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let rd = fs::read_dir(&sdir).map_err(|e| format!("{}: {e}", sdir.display()))?;
        for dirent in rd.flatten() {
            let sp = dirent.path();
            let Some(name) = sp.file_name() else { continue };
            let dp = ddir.join(name);
            let Ok(m) = fs::symlink_metadata(&sp) else { continue };
            if m.is_symlink() {
                recreate_symlink(&sp, &dp)?;
            } else if m.is_dir() {
                fs::create_dir_all(&dp).map_err(|e| format!("{}: {e}", dp.display()))?;
                stack.push((sp, dp));
            } else if !copy_file_chunked(&sp, &dp, done, total, progress, cancel)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Recreate a symlink at `dst` pointing wherever `src` points.
/// Links are never followed (same stance as the disk-usage walker) —
/// copying a folder of symlinks must not balloon into copying their
/// targets. [win-parity: symlink creation needs privilege; revisit]
fn recreate_symlink(src: &Path, dst: &Path) -> Result<(), String> {
    let target = fs::read_link(src).map_err(|e| format!("{}: {e}", src.display()))?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, dst).map_err(|e| format!("{}: {e}", dst.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = target;
        Err(format!(
            "symlink {} not copied (unsupported on this platform)",
            src.display()
        ))
    }
}

/// Copy every planned item into the destination under one collision
/// policy. Progress reports cumulative bytes against
/// `plan.total_bytes`.
pub fn run_copy(
    plan: &OpPlan,
    policy: CollisionPolicy,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<OpOutcome, String> {
    let mut outcome = OpOutcome::default();
    let mut done = 0u64;
    for src in &plan.sources {
        if cancel.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            return Ok(outcome);
        }
        let Some(dst) = resolve_dest(src, &plan.dest_dir, policy, &mut outcome)? else {
            continue;
        };
        if !copy_item(src, &dst, &mut done, plan.total_bytes, progress, cancel)? {
            outcome.cancelled = true;
            return Ok(outcome);
        }
        outcome.created.push((src.clone(), dst));
    }
    Ok(outcome)
}

/// Move every planned item. Same-volume items take the `rename` fast
/// path (instant — their planned bytes tick through progress in one
/// jump); cross-volume items copy then delete the source, and the
/// delete only runs when that item's copy fully succeeded.
pub fn run_move(
    plan: &OpPlan,
    policy: CollisionPolicy,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<OpOutcome, String> {
    let mut outcome = OpOutcome::default();
    let mut done = 0u64;
    for src in &plan.sources {
        if cancel.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            return Ok(outcome);
        }
        let Some(dst) = resolve_dest(src, &plan.dest_dir, policy, &mut outcome)? else {
            continue;
        };
        if same_volume(src, &plan.dest_dir) {
            fs::rename(src, &dst).map_err(|e| format!("{}: {e}", src.display()))?;
            // Credit the whole item's bytes in one jump.
            done = done.saturating_add(item_bytes(src, &dst));
            progress(done, plan.total_bytes);
        } else {
            if !copy_item(src, &dst, &mut done, plan.total_bytes, progress, cancel)? {
                outcome.cancelled = true;
                return Ok(outcome);
            }
            let meta =
                fs::symlink_metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
            let removed = if meta.is_dir() && !meta.is_symlink() {
                fs::remove_dir_all(src)
            } else {
                fs::remove_file(src)
            };
            removed.map_err(|e| format!("remove {}: {e}", src.display()))?;
        }
        outcome.created.push((src.clone(), dst));
    }
    Ok(outcome)
}

/// Size credit for a renamed item: post-rename the source is gone, so
/// measure the destination (files only; a walk would be wasted work
/// for a progress jump, so directories credit 0 and the bar simply
/// finishes early — rename is instant anyway).
fn item_bytes(_src: &Path, dst: &Path) -> u64 {
    fs::symlink_metadata(dst)
        .map(|m| if m.is_dir() { 0 } else { m.len() })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "feraille-fileops-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(p: &Path, contents: &[u8]) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, contents).unwrap();
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn copies_a_tree_with_progress() {
        let root = scratch("copy-tree");
        let src = root.join("src/proj");
        write(&src.join("a.txt"), b"hello");
        write(&src.join("sub/b.bin"), &[7u8; 1000]);
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();

        let cancel = no_cancel();
        let plan = plan_transfer(std::slice::from_ref(&src), &dest, &cancel).unwrap();
        assert_eq!(plan.total_bytes, 1005);
        assert!(plan.conflicts.is_empty());

        let mut last = (0, 0);
        let out = run_copy(&plan, CollisionPolicy::KeepBoth, &mut |d, t| last = (d, t), &cancel)
            .unwrap();
        assert!(!out.cancelled);
        assert_eq!(out.created.len(), 1);
        assert_eq!(last, (1005, 1005));
        assert_eq!(fs::read(dest.join("proj/a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(dest.join("proj/sub/b.bin")).unwrap().len(), 1000);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn keep_both_picks_numbered_names() {
        let root = scratch("keep-both");
        let src = root.join("src/a.txt");
        write(&src, b"new");
        let dest = root.join("dest");
        write(&dest.join("a.txt"), b"old");
        write(&dest.join("a 2.txt"), b"older");

        let cancel = no_cancel();
        let plan = plan_transfer(&[src], &dest, &cancel).unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        let out = run_copy(&plan, CollisionPolicy::KeepBoth, &mut |_, _| {}, &cancel).unwrap();
        assert_eq!(out.created[0].1, dest.join("a 3.txt"));
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"old");
        assert_eq!(fs::read(dest.join("a 3.txt")).unwrap(), b"new");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn replace_and_skip_policies() {
        let root = scratch("replace-skip");
        let src_a = root.join("src/a.txt");
        let src_b = root.join("src/b.txt");
        write(&src_a, b"new-a");
        write(&src_b, b"new-b");
        let dest = root.join("dest");
        write(&dest.join("a.txt"), b"old-a");
        write(&dest.join("b.txt"), b"old-b");

        let cancel = no_cancel();
        let plan = plan_transfer(std::slice::from_ref(&src_a), &dest, &cancel).unwrap();
        let out = run_copy(&plan, CollisionPolicy::Replace, &mut |_, _| {}, &cancel).unwrap();
        assert_eq!(out.replaced, 1);
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"new-a");

        let plan = plan_transfer(&[src_b], &dest, &cancel).unwrap();
        let out = run_copy(&plan, CollisionPolicy::Skip, &mut |_, _| {}, &cancel).unwrap();
        assert_eq!(out.skipped, 1);
        assert!(out.created.is_empty());
        assert_eq!(fs::read(dest.join("b.txt")).unwrap(), b"old-b");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_mid_batch_keeps_completed_items() {
        let root = scratch("cancel");
        let src_a = root.join("src/a.bin");
        let src_b = root.join("src/b.bin");
        write(&src_a, &[1u8; 500]);
        write(&src_b, &[2u8; 500]);
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();

        let cancel = AtomicBool::new(false);
        let plan = plan_transfer(&[src_a, src_b], &dest, &cancel).unwrap();
        // Flip cancel from inside the progress callback after the
        // first item's bytes have landed.
        let out = run_copy(
            &plan,
            CollisionPolicy::KeepBoth,
            &mut |done, _| {
                if done >= 500 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            &cancel,
        )
        .unwrap();
        assert!(out.cancelled);
        assert_eq!(out.created.len(), 1);
        // Exactly one of the two made it; no half-written second file.
        let survivors: Vec<_> = fs::read_dir(&dest).unwrap().flatten().collect();
        assert_eq!(survivors.len(), 1);
        assert_eq!(fs::read(survivors[0].path()).unwrap().len(), 500);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn move_renames_on_same_volume() {
        let root = scratch("move");
        let src = root.join("src/dir");
        write(&src.join("f.txt"), b"payload");
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();
        assert!(same_volume(&src, &dest));

        let cancel = no_cancel();
        let plan = plan_transfer(std::slice::from_ref(&src), &dest, &cancel).unwrap();
        let out = run_move(&plan, CollisionPolicy::KeepBoth, &mut |_, _| {}, &cancel).unwrap();
        assert!(!src.exists());
        assert_eq!(fs::read(dest.join("dir/f.txt")).unwrap(), b"payload");
        assert_eq!(out.created, vec![(src, dest.join("dir"))]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_rejects_dest_inside_source() {
        let root = scratch("self-copy");
        let src = root.join("outer");
        let dest = src.join("inner");
        fs::create_dir_all(&dest).unwrap();
        let cancel = no_cancel();
        let err = plan_transfer(&[src], &dest, &cancel).unwrap_err();
        assert!(err.contains("into itself"), "{err}");
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_recreated_not_followed() {
        let root = scratch("symlink");
        let src = root.join("src/dir");
        write(&src.join("real.txt"), b"data");
        std::os::unix::fs::symlink("real.txt", src.join("link")).unwrap();
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();

        let cancel = no_cancel();
        let plan = plan_transfer(&[src], &dest, &cancel).unwrap();
        // The link itself contributes its target-path length, not the
        // target's bytes — links are never followed.
        run_copy(&plan, CollisionPolicy::KeepBoth, &mut |_, _| {}, &cancel).unwrap();
        let copied_link = dest.join("dir/link");
        assert!(fs::symlink_metadata(&copied_link).unwrap().is_symlink());
        assert_eq!(fs::read_link(&copied_link).unwrap(), PathBuf::from("real.txt"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pick_available_name_schemes() {
        let root = scratch("naming");
        write(&root.join("a.txt"), b"x");
        write(&root.join("a 2.txt"), b"x");
        assert_eq!(
            pick_available_name(&root, OsStr::new("a.txt"), NameScheme::Numbered).unwrap(),
            root.join("a 3.txt")
        );
        assert_eq!(
            pick_available_name(&root, OsStr::new("a.txt"), NameScheme::Copy).unwrap(),
            root.join("a copy.txt")
        );
        let _ = fs::remove_dir_all(&root);
    }
}
