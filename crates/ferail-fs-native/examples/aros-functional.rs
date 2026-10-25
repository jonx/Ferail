// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper
//! Production filesystem/archive API qualification, separate from GPUI interaction tests.
//! Usage: aros-functional NEW_ROOT [SECOND_NEW_ROOT]. Roots must not exist.
//! Fixtures are retained for inspection; no volume-root Trashcan is touched.
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use ferail_archive::Format;
use ferail_core::{EntryKind, FsBackend};
use ferail_fs_native::file_ops::{self, CollisionPolicy, TransferProgress};
use ferail_fs_native::{archive, NativeFs};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn require(ok: bool, message: &str) -> Result {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn bytes(path: &Path, expected: &[u8]) -> Result {
    require(
        fs::read(path)? == expected,
        &format!("byte mismatch: {}", path.display()),
    )
}
// Fixture bytes are independent of production encoding helpers: AROS names
// use one ISO-8859-1 byte per character; archive labels remain Unicode.
fn native_fixture_path(value: &str) -> std::ffi::OsString {
    #[cfg(target_os = "aros")]
    {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(
            value
                .chars()
                .map(|ch| {
                    assert!(
                        u32::from(ch) <= 255,
                        "fixture must be representable in Latin-1"
                    );
                    ch as u8
                })
                .collect(),
        )
    }
    #[cfg(not(target_os = "aros"))]
    {
        value.into()
    }
}
fn fresh(root: &Path, name: &str) -> Result<PathBuf> {
    let path = root.join(native_fixture_path(name));
    fs::create_dir(&path)?;
    Ok(path)
}
fn transfer(
    src: &[PathBuf],
    dest: &Path,
    policy: CollisionPolicy,
    moving: bool,
) -> Result<file_ops::OpOutcome> {
    let progress = TransferProgress::new();
    let cancel = AtomicBool::new(false);
    let plan = file_ops::plan_transfer(src, dest, &progress, &cancel)?;
    progress.begin_transfer(plan.total_bytes, plan.total_items);
    let out = if moving {
        file_ops::run_move(&plan, &|_| policy, &progress, &cancel)?
    } else {
        file_ops::run_copy(&plan, &|_| policy, &progress, &cancel)?
    };
    require(
        !out.cancelled && !out.has_failures(),
        &format!("transfer failures {:?}", out.failed),
    )?;
    Ok(out)
}
struct Suite {
    passed: usize,
    failed: usize,
}
impl Suite {
    fn case(&mut self, name: &str, test: impl FnOnce() -> Result) {
        match test() {
            Ok(()) => {
                self.passed += 1;
                println!("FERAIL_FUNCTIONAL case={name} result=PASS");
            }
            Err(error) => {
                self.failed += 1;
                println!(
                    "FERAIL_FUNCTIONAL case={name} result=FAIL detail={:?}",
                    error.to_string()
                );
            }
        }
    }
}
fn native_cases(s: &mut Suite, root: &Path) {
    s.case("native-enumerate-metadata-latin1", || {
        let dir = fresh(root, "listing espaces é")?;
        let nested = fresh(&dir, "sous dossier")?;
        fs::write(
            dir.join(native_fixture_path("texte été.txt")),
            b"native payload",
        )?;
        fs::write(nested.join("child"), b"child")?;
        let backend = NativeFs::new();
        let id = backend.id_for_path(&dir);
        require(
            backend.path_for(id).as_deref() == Some(dir.as_path()),
            "identity roundtrip",
        )?;
        let listing = backend.enumerate(id);
        require(
            listing.error.is_none() && listing.initial.len() == 2,
            "nonempty exact listing",
        )?;
        require(
            listing.initial.iter().any(|e| {
                e.name.as_ref() == "texte été.txt" && e.kind == EntryKind::File && e.size == 14
            }),
            "file row metadata",
        )?;
        require(
            listing
                .initial
                .iter()
                .any(|e| e.name.as_ref() == "sous dossier" && e.kind == EntryKind::Directory),
            "directory row",
        )?;
        let row = backend
            .file_entry_for_path(&dir.join(native_fixture_path("texte été.txt")))
            .ok_or("missing file metadata")?;
        require(
            row.size == 14 && row.name.as_ref() == "texte été.txt",
            "metadata name and size",
        )?;
        let mut streamed = Vec::new();
        let error = backend.enumerate_streaming(&dir, 1, &AtomicBool::new(false), |batch| {
            streamed.extend(batch);
        });
        require(
            error.is_none() && streamed.len() == listing.initial.len(),
            "streaming listing parity",
        )?;
        require(
            streamed
                .iter()
                .any(|entry| entry.name.as_ref() == "texte été.txt" && entry.size == 14),
            "streaming Latin-1 metadata",
        )?;
        let mut paths = Vec::new();
        ferail_fs_native::list_subtree_paths(&dir, true, &mut paths);
        require(
            paths.len() == 3 && paths.contains(&nested.join("child")),
            "recursive traversal",
        )
    });
    s.case("native-streaming-batches", || {
        let dir = fresh(root, "streaming")?;
        for n in 0..7 {
            fs::write(dir.join(format!("{n}.txt")), [n])?;
        }
        let mut names = Vec::new();
        let mut batches = 0;
        let err = NativeFs::new().enumerate_streaming(&dir, 2, &AtomicBool::new(false), |batch| {
            batches += 1;
            names.extend(batch.into_iter().map(|e| e.name.to_string()));
        });
        names.sort();
        names.dedup();
        require(
            err.is_none() && names.len() == 7 && batches == 4,
            "streaming exact rows/batches",
        )
    });
    s.case("native-streaming-cancel", || {
        let dir = root.join("streaming");
        let cancel = AtomicBool::new(false);
        let mut seen = 0;
        let err = NativeFs::new().enumerate_streaming(&dir, 1, &cancel, |batch| {
            seen += batch.len();
            cancel.store(true, Ordering::Relaxed);
        });
        require(
            err.is_none() && seen == 1,
            "stream cancellation must stop after first row",
        )
    });
    s.case("native-missing-error", || {
        let backend = NativeFs::new();
        let missing = root.join("missing directory");
        let listing = backend.enumerate(backend.id_for_path(&missing));
        require(
            listing.error.is_some() && listing.initial.is_empty(),
            "missing directory must report error",
        )?;
        require(
            backend.file_entry_for_path(&missing).is_none(),
            "missing metadata must be absent",
        )
    });
}
fn file_cases(s: &mut Suite, root: &Path) {
    s.case("file-create-no-clobber", || {
        let dir = fresh(root, "new files")?;
        let a = file_ops::write_new_file(&dir, "été spaces", "txt", b"first")?;
        let b = file_ops::write_new_file(&dir, "été spaces", "txt", b"second")?;
        require(a != b, "new file collision")?;
        require(
            a.file_name() == Some(native_fixture_path("été spaces.txt").as_os_str()),
            "typed name must use native filename bytes",
        )?;
        bytes(&a, b"first")?;
        bytes(&b, b"second")
    });
    for policy in [
        CollisionPolicy::Skip,
        CollisionPolicy::KeepBoth,
        CollisionPolicy::Replace,
    ] {
        let tag = match policy {
            CollisionPolicy::Skip => "skip",
            CollisionPolicy::KeepBoth => "keep-both",
            CollisionPolicy::Replace => "replace",
        };
        s.case(&format!("file-copy-collision-{tag}"), || {
            let base = fresh(root, tag)?;
            let source = fresh(&base, "source")?;
            let dest = fresh(&base, "dest")?;
            let src = source.join(native_fixture_path("document été.txt"));
            fs::write(&src, b"new source")?;
            fs::write(
                dest.join(native_fixture_path("document été.txt")),
                b"old destination",
            )?;
            let out = transfer(std::slice::from_ref(&src), &dest, policy, false)?;
            bytes(&src, b"new source")?;
            match policy {
                CollisionPolicy::Skip => {
                    require(out.skipped == 1 && out.created.is_empty(), "skip outcome")?;
                    bytes(
                        &dest.join(native_fixture_path("document été.txt")),
                        b"old destination",
                    )
                }
                CollisionPolicy::KeepBoth => {
                    require(
                        out.created.len() == 1 && out.replaced == 0,
                        "keep both outcome",
                    )?;
                    bytes(
                        &dest.join(native_fixture_path("document été.txt")),
                        b"old destination",
                    )?;
                    require(
                        out.created[0].1 != dest.join(native_fixture_path("document été.txt")),
                        "distinct destination",
                    )?;
                    bytes(&out.created[0].1, b"new source")
                }
                CollisionPolicy::Replace => {
                    require(
                        out.replaced == 1 && out.created.len() == 1,
                        "replace outcome",
                    )?;
                    bytes(
                        &dest.join(native_fixture_path("document été.txt")),
                        b"new source",
                    )
                }
            }
        });
    }
    s.case("file-copy-tree-and-move", || {
        let base = fresh(root, "tree transfers")?;
        let source = fresh(&base, "tree")?;
        let nested = fresh(&source, "nested spaces")?;
        fs::write(nested.join("payload.bin"), [0, 1, 255, 42])?;
        fresh(&source, "empty")?;
        let dest = fresh(&base, "copied")?;
        let moved = fresh(&base, "moved")?;
        let out = transfer(
            std::slice::from_ref(&source),
            &dest,
            CollisionPolicy::Skip,
            false,
        )?;
        require(
            out.created.len() == 1 && source.exists(),
            "copy tree outcome",
        )?;
        bytes(
            &dest.join("tree/nested spaces/payload.bin"),
            &[0, 1, 255, 42],
        )?;
        require(
            dest.join("tree/empty").is_dir(),
            "empty directory preserved",
        )?;
        let out = transfer(&[dest.join("tree")], &moved, CollisionPolicy::Skip, true)?;
        require(
            out.created.len() == 1 && !dest.join("tree").exists(),
            "move removes source",
        )?;
        bytes(
            &moved.join("tree/nested spaces/payload.bin"),
            &[0, 1, 255, 42],
        )
    });
    s.case("file-cancel-before-copy-and-move", || {
        let base = fresh(root, "cancel before")?;
        let src = base.join("source");
        fs::write(&src, b"preserve")?;
        let dest = fresh(&base, "dest")?;
        let progress = TransferProgress::new();
        let plan = file_ops::plan_transfer(
            std::slice::from_ref(&src),
            &dest,
            &progress,
            &AtomicBool::new(false),
        )?;
        let cancel = AtomicBool::new(true);
        for moving in [false, true] {
            let out = if moving {
                file_ops::run_move(&plan, &|_| CollisionPolicy::Replace, &progress, &cancel)?
            } else {
                file_ops::run_copy(&plan, &|_| CollisionPolicy::Replace, &progress, &cancel)?
            };
            require(out.cancelled && out.created.is_empty(), "cancelled outcome")?;
            bytes(&src, b"preserve")?;
            require(
                fs::read_dir(&dest)?.next().is_none(),
                "cancel must not create destination",
            )?;
        }
        Ok(())
    });
    s.case("file-cancel-mid-batch", || {
        let base = fresh(root, "cancel batch")?;
        let source = fresh(&base, "source")?;
        let dest = fresh(&base, "dest")?;
        let mut sources = Vec::new();
        for n in 0..3 {
            let p = source.join(format!("{n}.bin"));
            fs::write(&p, [n as u8; 257])?;
            sources.push(p);
        }
        let progress = TransferProgress::new();
        let cancel = AtomicBool::new(false);
        let plan = file_ops::plan_transfer(&sources, &dest, &progress, &cancel)?;
        // The policy callback runs for every top-level item. Cancel when one
        // item is already complete, without a timing-sensitive helper thread.
        let out = file_ops::run_copy(
            &plan,
            &|_| {
                if progress.items_done() >= 1 {
                    cancel.store(true, Ordering::Relaxed);
                }
                CollisionPolicy::Skip
            },
            &progress,
            &cancel,
        )?;
        require(
            out.cancelled && out.created.len() == 1 && !out.has_failures(),
            "partial outcome",
        )?;
        bytes(&dest.join("0.bin"), &[0; 257])?;
        require(!dest.join("2.bin").exists(), "later item must not start")?;
        #[cfg(not(target_os = "macos"))]
        require(
            !dest.join("1.bin").exists(),
            "partial current file must be removed",
        )?;
        for (n, path) in sources.iter().enumerate() {
            bytes(path, &[n as u8; 257])?;
        }
        Ok(())
    });
    s.case("file-invalid-transfer-plans", || {
        let dir = fresh(root, "invalid plans")?;
        let progress = TransferProgress::new();
        let cancel = AtomicBool::new(false);
        require(
            file_ops::plan_transfer(&[dir.join("missing")], &dir, &progress, &cancel).is_err(),
            "missing source",
        )?;
        let child = fresh(&dir, "child")?;
        require(
            file_ops::plan_transfer(std::slice::from_ref(&dir), &child, &progress, &cancel)
                .is_err(),
            "copy into self must fail",
        )
    });
    s.case("native-rename-no-clobber-contract", || {
        use ferail_fs_native::rename::rename_no_replace;
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let dir = fresh(root, "rename contract")?;
        let a = dir.join("source.txt");
        let b = dir.join("existing.txt");
        fs::write(&a, b"source exact bytes")?;
        fs::write(&b, b"destination exact bytes")?;
        let error = rename_no_replace(&a, &b)
            .err()
            .ok_or("collision must refuse")?;
        require(
            error.kind() == std::io::ErrorKind::AlreadyExists,
            "file collision reports AlreadyExists",
        )?;
        bytes(&a, b"source exact bytes")?;
        bytes(&b, b"destination exact bytes")?;
        let moved = dir.join("moved.txt");
        rename_no_replace(&a, &moved)?;
        require(!a.exists(), "successful rename removes old name")?;
        fs::write(&a, b"new occupant")?;
        require(
            rename_no_replace(&moved, &a).is_err(),
            "undo collision must refuse",
        )?;
        bytes(&a, b"new occupant")?;
        bytes(&moved, b"source exact bytes")?;
        let from = fresh(&dir, "source-dir")?;
        let to = fresh(&dir, "existing-dir")?;
        fs::write(from.join("member"), b"source directory member")?;
        fs::write(to.join("member"), b"destination directory member")?;
        require(
            rename_no_replace(&from, &to).is_err(),
            "directory collision must refuse",
        )?;
        bytes(&from.join("member"), b"source directory member")?;
        bytes(&to.join("member"), b"destination directory member")?;
        let renamed_dir = dir.join("renamed-dir");
        rename_no_replace(&from, &renamed_dir)?;
        bytes(&renamed_dir.join("member"), b"source directory member")?;
        let upper = dir.join("MOVED.TXT");
        rename_no_replace(&moved, &upper)?;
        bytes(&upper, b"source exact bytes")?;
        require(
            fs::read_dir(&dir)?
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().as_bytes() == b"MOVED.TXT"),
            "case-only rename must update stored name",
        )?;
        let raw = dir.join(OsStr::from_bytes(b"legacy-\xe9.txt"));
        let raw_target = dir.join(OsStr::from_bytes(b"renamed-\xe9.txt"));
        fs::write(&raw, b"raw DOS filename")?;
        rename_no_replace(&raw, &raw_target)?;
        bytes(&raw_target, b"raw DOS filename")?;
        for bad in [
            b"evil:assign".as_slice(),
            b"bad\0name".as_slice(),
            b"..".as_slice(),
        ] {
            let error = rename_no_replace(&upper, &dir.join(OsStr::from_bytes(bad)))
                .err()
                .ok_or("invalid destination must refuse")?;
            require(
                error.kind() == std::io::ErrorKind::InvalidInput,
                "invalid leaf classification",
            )?;
            bytes(&upper, b"source exact bytes")?;
        }
        Ok(())
    });
    s.case("native-rename-symbolic-link-preservation", || {
        use ferail_fs_native::rename::rename_no_replace;
        use std::os::unix::fs::symlink;
        let dir = fresh(root, "rename symbolic links")?;
        let referent = dir.join("referent.txt");
        fs::write(&referent, b"referent must not move")?;
        let missing = dir.join("missing-referent");
        let mut outcomes = Vec::new();
        for (name, target) in [("live", &referent), ("dangling", &missing)] {
            let link = dir.join(format!("original-{name}"));
            let renamed = dir.join(format!("renamed-{name}"));
            symlink(target, &link).map_err(|e| format!("create {name} symlink: {e}"))?;
            let stored_target = fs::read_link(&link)?;
            let supported = match rename_no_replace(&link, &renamed) {
                Ok(()) => true,
                // RAM handler GetHardObject explicitly rejects soft links.
                // Record that capability; preserve the link and its referent.
                Err(error) if error.raw_os_error() == Some(1233) => false,
                Err(error) => return Err(format!("rename {name} symlink: {error}").into()),
            };
            let remaining = if supported { &renamed } else { &link };
            let absent = if supported { &link } else { &renamed };
            require(
                fs::symlink_metadata(remaining)?.file_type().is_symlink(),
                "item must remain a symbolic link",
            )?;
            require(
                fs::read_link(remaining)? == stored_target,
                "stored link target bytes preserved",
            )?;
            require(
                fs::symlink_metadata(absent).is_err(),
                "other link name must be absent",
            )?;
            bytes(&referent, b"referent must not move")?;
            require(!missing.exists(), "dangling referent never created")?;
            outcomes.push(supported);
        }
        require(
            outcomes[0] == outcomes[1],
            "live and dangling rename capability differs",
        )?;
        if outcomes[0] {
            println!("FERAIL_CAPABILITY feature=rename-symbolic-links result=SUPPORTED");
        } else {
            println!(
                "FERAIL_CAPABILITY feature=rename-symbolic-links result=UNSUPPORTED errno=1233"
            );
        }
        Ok(())
    });
    s.case("std-ui-delete-primitives", || {
        // UI deletion shares these std filesystem primitives; rename has its own API case.
        let dir = fresh(root, "delete primitives")?;
        let path = dir.join(native_fixture_path("delete é"));
        fs::write(&path, b"preserved until deletion")?;
        bytes(&path, b"preserved until deletion")?;
        fs::remove_file(&path)?;
        fs::remove_dir(&dir)?;
        require(!dir.exists(), "delete file and directory")
    });
}
fn archive_cases(s: &mut Suite, root: &Path) {
    for (format, suffix) in [
        (Format::Zip, "zip"),
        (Format::Tar, "tar"),
        (Format::TarGz, "tar.gz"),
        (Format::TarBz2, "tar.bz2"),
        (Format::TarXz, "tar.xz"),
        (Format::Gzip, "gz"),
        (Format::Bzip2, "bz2"),
        (Format::Xz, "xz"),
    ] {
        s.case(&format!("archive-roundtrip-{suffix}"), || {
            let dir = fresh(root, &format!("archive-{suffix}"))?;
            let source = dir.join(native_fixture_path("payload été.bin"));
            let payload = [0, 255, 1, 2, 3, 0, 127];
            fs::write(&source, payload)?;
            let output = dir.join(format!("payload.{suffix}"));
            let progress = TransferProgress::new();
            let cancel = AtomicBool::new(false);
            archive::create_archive(
                format,
                &[source.as_path()],
                &output,
                archive::CreateOptions::default(),
                &progress,
                &cancel,
            )?;
            require(
                archive::probe_format(&output) == Some(format),
                "format detection",
            )?;
            let toc = archive::read_toc(&output, None)?;
            require(
                toc.file_count() == 1 && toc.entries.len() == 1,
                "nonempty exact TOC",
            )?;
            let member = &toc.entries[0].path;
            if !format.is_single_member() {
                require(
                    member == "payload été.bin",
                    "archive label must be Unicode without byte loss",
                )?;
                require(
                    archive::read_entry_bytes(&output, member, None, 1024)?
                        == Some(payload.to_vec()),
                    "member bytes",
                )?;
                require(
                    archive::read_entry_bytes(&output, member, None, 2)?.is_none(),
                    "bounded member read",
                )?;
            }
            let dest = fresh(&dir, "extract")?;
            let out = archive::extract_all(
                &output,
                &dest,
                archive::ExtractOptions {
                    password: None,
                    overwrite: false,
                },
                &TransferProgress::new(),
                &cancel,
            )?;
            require(
                out.files_written == 1 && out.skipped.is_empty(),
                "extraction outcome",
            )?;
            bytes(&dest.join(native_fixture_path(member)), &payload)?;
            // The same extraction may not clobber a user edit by default.
            fs::write(dest.join(native_fixture_path(member)), b"edited")?;
            let out = archive::extract_all(
                &output,
                &dest,
                archive::ExtractOptions {
                    password: None,
                    overwrite: false,
                },
                &TransferProgress::new(),
                &cancel,
            )?;
            require(
                out.files_written == 0 && out.skipped.len() == 1,
                "extract skip existing",
            )?;
            bytes(&dest.join(native_fixture_path(member)), b"edited")
        });
    }
    s.case("archive-zip-traversal-guard", || {
        let dir = fresh(root, "unsafe archive")?;
        let output = dir.join("hostile.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&output)?);
        for name in [
            "../escaped",
            "nested/../../escaped2",
            "C:/escaped",
            "safe.txt",
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())?;
            zip.write_all(b"fixture")?;
        }
        zip.finish()?;
        let dest = fresh(&dir, "extract")?;
        let out = archive::extract_all(
            &output,
            &dest,
            archive::ExtractOptions {
                password: None,
                overwrite: true,
            },
            &TransferProgress::new(),
            &AtomicBool::new(false),
        )?;
        require(
            out.files_written == 1 && out.skipped.len() == 3,
            "three unsafe paths must be skipped",
        )?;
        require(
            out.skipped
                .iter()
                .all(|e| e.reason == archive::SkipReason::UnsafePath),
            "unsafe skip reasons",
        )?;
        bytes(&dest.join("safe.txt"), b"fixture")?;
        require(
            !dir.join("escaped").exists() && !dir.join("escaped2").exists(),
            "no escape writes",
        )
    });
    s.case("archive-unrepresentable-native-name", || {
        let dir = fresh(root, "archive native names")?;
        let output = dir.join("unicode.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&output)?);
        for name in ["safe.txt", "non-latin-\u{1f680}.txt"] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())?;
            zip.write_all(b"fixture")?;
        }
        zip.finish()?;
        let dest = fresh(&dir, "extract")?;
        let result = archive::extract_all(
            &output,
            &dest,
            archive::ExtractOptions {
                password: None,
                overwrite: false,
            },
            &TransferProgress::new(),
            &AtomicBool::new(false),
        );
        require(result.is_err(), "unrepresentable names must be reported")?;
        require(
            fs::read_dir(&dest)?.next().is_none(),
            "name preflight must reject before creating any output",
        )
    });
    s.case("archive-aros-volume-path-guard", || {
        // Test the validator without attempting an out-of-fixture write.
        // AROS volume/assign roots must be rejected before filesystem I/O.
        for name in ["SYS:escaped", "RAM:escaped", "T:escaped", "dir/SYS:escaped"] {
            require(
                ferail_archive::safe_relative_path(name).is_err(),
                "AROS volume/assign entry must be rejected",
            )?;
        }
        Ok(())
    });
    s.case("archive-corrupt-error", || {
        let p = root.join("corrupt.zip");
        fs::write(&p, b"not a zip")?;
        require(
            archive::read_toc(&p, None).is_err(),
            "corrupt archive must report an error",
        )
    });
    s.case("archive-password-and-cancellation", || {
        let dir = fresh(root, "encrypted archive")?;
        let source = dir.join("secret.txt");
        fs::write(&source, b"private fixture bytes")?;
        let output = dir.join("secret.zip");
        archive::create_archive(
            Format::Zip,
            &[source.as_path()],
            &output,
            archive::CreateOptions {
                password: Some("test-password"),
                ..Default::default()
            },
            &TransferProgress::new(),
            &AtomicBool::new(false),
        )?;
        require(
            archive::read_toc(&output, None)?.needs_password,
            "encrypted TOC flag",
        )?;
        for (index, password) in [None, Some("wrong-password")].into_iter().enumerate() {
            let dest = fresh(&dir, &format!("refused-{index}"))?;
            require(
                archive::extract_all(
                    &output,
                    &dest,
                    archive::ExtractOptions {
                        password,
                        overwrite: true,
                    },
                    &TransferProgress::new(),
                    &AtomicBool::new(false),
                )
                .is_err(),
                "missing or wrong password must fail",
            )?;
            require(
                !dest.join("secret.txt").exists(),
                "failed decryption must not publish a file",
            )?;
        }
        let dest = fresh(&dir, "decrypted")?;
        let outcome = archive::extract_all(
            &output,
            &dest,
            archive::ExtractOptions {
                password: Some("test-password"),
                overwrite: false,
            },
            &TransferProgress::new(),
            &AtomicBool::new(false),
        )?;
        require(outcome.files_written == 1, "decrypted file count")?;
        bytes(&dest.join("secret.txt"), b"private fixture bytes")?;
        let cancelled = dir.join("cancelled.zip");
        require(
            archive::create_archive(
                Format::Zip,
                &[source.as_path()],
                &cancelled,
                archive::CreateOptions::default(),
                &TransferProgress::new(),
                &AtomicBool::new(true),
            )
            .is_err(),
            "pre-cancelled archive creation must fail",
        )?;
        require(
            !cancelled.exists(),
            "cancelled archive must not be published",
        )
    });
    s.case("archive-conversion-no-clobber", || {
        let dir = fresh(root, "conversion")?;
        let source = dir.join(native_fixture_path("text é.txt"));
        fs::write(&source, b"conversion exact payload")?;
        let output = dir.join("original.zip");
        archive::create_archive(
            Format::Zip,
            &[source.as_path()],
            &output,
            archive::CreateOptions::default(),
            &TransferProgress::new(),
            &AtomicBool::new(false),
        )?;
        let opts = archive::ConvertOptions {
            target: Format::TarGz,
            level: Default::default(),
            input_password: None,
            output_password: None,
        };
        let first = archive::convert_archive(
            &output,
            "converted",
            opts,
            &TransferProgress::new(),
            &AtomicBool::new(false),
        )?;
        let first_bytes = fs::read(&first.output)?;
        let second = archive::convert_archive(
            &output,
            "converted",
            opts,
            &TransferProgress::new(),
            &AtomicBool::new(false),
        )?;
        require(
            first.output != second.output
                && first.files_converted == 1
                && second.files_converted == 1,
            "conversion must select a fresh destination",
        )?;
        bytes(&first.output, &first_bytes)?;
        require(
            archive::read_entry_bytes(&second.output, "text é.txt", None, 1024)?
                == Some(b"conversion exact payload".to_vec()),
            "converted member bytes",
        )?;
        require(
            archive::convert_archive(
                &output,
                "cancelled",
                opts,
                &TransferProgress::new(),
                &AtomicBool::new(true),
            )
            .is_err(),
            "cancelled conversion must fail",
        )?;
        require(
            !dir.join("cancelled.tar.gz").exists(),
            "cancelled conversion must not publish",
        )
    });
    s.case("archive-sevenz-read-extract", || {
        let dir = fresh(root, "sevenz")?;
        let source = dir.join("fixture.txt");
        fs::write(&source, b"7z exact payload")?;
        let output = dir.join("fixture.7z");
        // Generate this fixture directly through the codec writer, then test Ferail decoding.
        sevenz_rust::compress_to_path(&source, &output)?;
        let toc = archive::read_toc(&output, None)?;
        require(toc.file_count() == 1, "7z exact TOC")?;
        let member = &toc
            .entries
            .iter()
            .find(|e| !e.is_dir)
            .ok_or("7z member missing")?
            .path;
        let dest = fresh(&dir, "extracted")?;
        let out = archive::extract_all(
            &output,
            &dest,
            archive::ExtractOptions {
                password: None,
                overwrite: false,
            },
            &TransferProgress::new(),
            &AtomicBool::new(false),
        )?;
        require(
            out.files_written == 1 && out.skipped.is_empty(),
            "7z extraction outcome",
        )?;
        bytes(&dest.join(native_fixture_path(member)), b"7z exact payload")
    });
    s.case("archive-lha-read-extract", || {
        let dir = fresh(root, "lha")?;
        let data = b"LHA exact payload";
        let name = b"fixture.txt";
        // Level-0 stored LHA fixture with an independently computed CRC-16/ARC.
        let mut crc = 0u16;
        for byte in data {
            crc ^= *byte as u16;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xa001
                } else {
                    crc >> 1
                };
            }
        }
        let mut body = b"-lh0-".to_vec();
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&0x0021_0000u32.to_le_bytes());
        body.extend_from_slice(&[0x20, 0, name.len() as u8]);
        body.extend_from_slice(name);
        body.extend_from_slice(&crc.to_le_bytes());
        let mut encoded = vec![
            body.len() as u8,
            body.iter().fold(0u8, |a, b| a.wrapping_add(*b)),
        ];
        encoded.extend_from_slice(&body);
        encoded.extend_from_slice(data);
        encoded.push(0);
        let output = dir.join("fixture.lha");
        fs::write(&output, encoded)?;
        require(
            archive::read_toc(&output, None)?.file_count() == 1,
            "LHA exact TOC",
        )?;
        let dest = fresh(&dir, "extracted")?;
        let out = archive::extract_all(
            &output,
            &dest,
            archive::ExtractOptions {
                password: None,
                overwrite: false,
            },
            &TransferProgress::new(),
            &AtomicBool::new(false),
        )?;
        require(
            out.files_written == 1 && out.skipped.is_empty(),
            "LHA extraction outcome",
        )?;
        bytes(&dest.join("fixture.txt"), data)
    });
}
fn cross_volume(s: &mut Suite, root: &Path, second: &Path) {
    s.case("file-cross-volume-copy-move", || {
        require(
            !file_ops::same_volume(root, second),
            "SECOND_NEW_ROOT must be on a different volume",
        )?;
        let source = fresh(root, "cross-volume source")?;
        let file = source.join(native_fixture_path("cross é.bin"));
        fs::write(&file, b"cross volume exact payload")?;
        let copied = fresh(second, "copied")?;
        let moved = fresh(second, "moved")?;
        let out = transfer(
            std::slice::from_ref(&file),
            &copied,
            CollisionPolicy::Skip,
            false,
        )?;
        require(out.created.len() == 1, "cross copy outcome")?;
        bytes(&file, b"cross volume exact payload")?;
        bytes(
            &copied.join(native_fixture_path("cross é.bin")),
            b"cross volume exact payload",
        )?;
        let out = transfer(
            std::slice::from_ref(&file),
            &moved,
            CollisionPolicy::Skip,
            true,
        )?;
        require(
            out.created.len() == 1 && !file.exists(),
            "cross move source removed",
        )?;
        bytes(
            &moved.join(native_fixture_path("cross é.bin")),
            b"cross volume exact payload",
        )
    });
}
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() || args.len() > 2 {
        println!(
            "FERAIL_FUNCTIONAL result=REFUSED reason=bad-arguments argc={} args={args:?}",
            args.len()
        );
        eprintln!("usage: aros-functional NEW_ROOT [SECOND_NEW_ROOT]");
        std::process::exit(2);
    }
    let roots: Vec<PathBuf> = args.into_iter().map(PathBuf::from).collect();
    // Validate every root before creating any fixture. Atomic create_dir below
    // also refuses a concurrent writer; never remove an existing tree.
    for root in &roots {
        match fs::symlink_metadata(root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Ok(metadata) => {
                println!("FERAIL_FUNCTIONAL result=REFUSED reason=existing-root root={root:?} directory={}", metadata.is_dir());
                std::process::exit(2);
            }
            Err(error) => {
                println!("FERAIL_FUNCTIONAL result=REFUSED reason=root-stat root={root:?} error={error:?}");
                std::process::exit(2);
            }
        }
    }
    for root in &roots {
        if let Err(e) = fs::create_dir(root) {
            println!(
                "FERAIL_FUNCTIONAL result=REFUSED reason=root-create root={root:?} error={e:?}"
            );
            std::process::exit(2);
        }
    }
    let mut suite = Suite {
        passed: 0,
        failed: 0,
    };
    native_cases(&mut suite, &roots[0]);
    file_cases(&mut suite, &roots[0]);
    archive_cases(&mut suite, &roots[0]);
    if roots.len() == 2 {
        cross_volume(&mut suite, &roots[0], &roots[1]);
    }
    println!(
        "FERAIL_FUNCTIONAL result={} passed={} failed={}",
        if suite.failed == 0 { "PASS" } else { "FAIL" },
        suite.passed,
        suite.failed
    );
    if suite.failed != 0 {
        std::process::exit(1);
    }
}
