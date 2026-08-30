//! Round-trip read tests: build a real archive with each backend, then read it
//! back through the codec layer and assert the TOC / summary. These exercise
//! the actual zip / tar / gzip encoders, not synthetic byte blobs, so a codec
//! or API drift shows up here.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use ferail_archive::Format;

use super::{
    archive_stamp, commit_archive_edits, convert_archive, create_archive, extract_all,
    extract_entries, format_of, materialize_archive_entry, probe_format, read_summary, read_toc,
    ArchiveAddition, ArchiveEditPlan, ArchiveError, ArchiveRename, ConvertOptions, CreateOptions,
    ExtractOptions, SkipReason,
};
use crate::file_ops::TransferProgress;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A unique temp directory that recursively removes itself on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "ferail-archive-dir-{}-{}-{}",
            std::process::id(),
            n,
            tag
        ));
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Fresh progress + non-cancelled flag for an extract call.
fn rig() -> (TransferProgress, AtomicBool) {
    (TransferProgress::new(), AtomicBool::new(false))
}

/// A unique temp path that removes itself on drop, so tests leave no litter.
struct TempFile(PathBuf);

impl TempFile {
    fn new(suffix: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "ferail-archive-{}-{}-{}",
            std::process::id(),
            n,
            suffix
        ));
        TempFile(p)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[test]
fn format_detection_routes_by_extension() {
    assert_eq!(
        format_of(std::path::Path::new("x.zip")).unwrap(),
        Format::Zip
    );
    assert_eq!(
        format_of(std::path::Path::new("x.tar.gz")).unwrap(),
        Format::TarGz
    );
    assert!(format_of(std::path::Path::new("x.txt")).is_err());
}

#[test]
fn zip_round_trip_toc_and_summary() {
    let tf = TempFile::new("roundtrip.zip");
    {
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let mut zw = zip::ZipWriter::new(File::create(tf.path()).unwrap());
        zw.add_directory("project/", opts).unwrap();
        zw.start_file("project/a.txt", opts).unwrap();
        zw.write_all(b"hello").unwrap();
        zw.start_file("project/sub/b.txt", opts).unwrap();
        zw.write_all(b"world!!").unwrap();
        zw.finish().unwrap();
    }

    let toc = read_toc(tf.path(), None).unwrap();
    assert_eq!(toc.file_count(), 2);
    assert_eq!(toc.single_root(), Some("project"));
    assert_eq!(toc.total_uncompressed(), Some(5 + 7));
    assert_eq!(toc.directory_count(), 1);
    assert!(toc.total_compressed().is_some());
    assert!(!toc.needs_password);
    for entry in toc.entries.iter().filter(|entry| !entry.is_dir) {
        assert_eq!(entry.compression_method.as_deref(), Some("Deflated"));
        assert!(entry
            .checksum
            .as_deref()
            .is_some_and(|checksum| checksum.starts_with("CRC32 ")));
    }

    let summary = read_summary(tf.path()).unwrap();
    assert_eq!(summary.file_count, Some(2));
    assert_eq!(summary.root.as_deref(), Some("project"));
    assert_eq!(summary.total_uncompressed, Some(12));
    assert!(!summary.encrypted);
}

#[test]
fn zip_multi_root_has_no_single_root() {
    let tf = TempFile::new("flat.zip");
    {
        let opts = zip::write::SimpleFileOptions::default();
        let mut zw = zip::ZipWriter::new(File::create(tf.path()).unwrap());
        zw.start_file("a.txt", opts).unwrap();
        zw.write_all(b"a").unwrap();
        zw.start_file("b.txt", opts).unwrap();
        zw.write_all(b"b").unwrap();
        zw.finish().unwrap();
    }
    let toc = read_toc(tf.path(), None).unwrap();
    assert_eq!(toc.single_root(), None);
    assert_eq!(toc.file_count(), 2);
}

#[test]
fn conversion_reuses_every_multi_file_writer_and_preserves_the_source() {
    const TARGETS: &[Format] = &[
        Format::Zip,
        Format::SevenZ,
        Format::Tar,
        Format::TarGz,
        Format::TarBz2,
        Format::TarXz,
    ];
    for target in TARGETS {
        let dir = TempDir::new(target.canonical_extension());
        let source = dir.path().join("source.zip");
        build_zip(
            &source,
            &[("project/a.txt", b"alpha"), ("project/sub/b.txt", b"beta")],
        );
        let original = fs::read(&source).unwrap();
        let (progress, cancel) = rig();
        let converted = convert_archive(
            &source,
            "converted",
            ConvertOptions {
                target: *target,
                level: Default::default(),
                input_password: None,
                output_password: None,
            },
            &progress,
            &cancel,
        )
        .unwrap_or_else(|error| panic!("{} conversion failed: {error}", target.label()));

        assert_eq!(converted.files_converted, 2);
        assert_eq!(format_of(&converted.output).unwrap(), *target);
        assert_eq!(read_toc(&converted.output, None).unwrap().file_count(), 2);
        assert_eq!(fs::read(&source).unwrap(), original);

        let extracted = dir.path().join("result");
        fs::create_dir(&extracted).unwrap();
        let (progress, cancel) = rig();
        extract_all(
            &converted.output,
            &extracted,
            ExtractOptions::default(),
            &progress,
            &cancel,
        )
        .unwrap();
        assert_eq!(fs::read(extracted.join("project/a.txt")).unwrap(), b"alpha");
        assert_eq!(
            fs::read(extracted.join("project/sub/b.txt")).unwrap(),
            b"beta"
        );
    }
}

#[test]
fn conversion_never_clobbers_and_cleans_cancelled_or_unsafe_work() {
    let dir = TempDir::new("convert-safety");
    let source = dir.path().join("source.zip");
    build_zip(&source, &[("a.txt", b"original")]);
    let original = fs::read(&source).unwrap();
    let (progress, cancel) = rig();
    let converted = convert_archive(
        &source,
        "source",
        ConvertOptions {
            target: Format::Zip,
            level: Default::default(),
            input_password: None,
            output_password: None,
        },
        &progress,
        &cancel,
    )
    .unwrap();
    assert_eq!(converted.output.file_name().unwrap(), "source 2.zip");
    assert_eq!(fs::read(&source).unwrap(), original);

    let cancel = AtomicBool::new(true);
    let progress = TransferProgress::new();
    assert!(matches!(
        convert_archive(
            &source,
            "cancelled",
            ConvertOptions {
                target: Format::TarGz,
                level: Default::default(),
                input_password: None,
                output_password: None,
            },
            &progress,
            &cancel,
        ),
        Err(ArchiveError::Cancelled)
    ));
    assert!(!dir.path().join("cancelled.tar.gz").exists());

    let unsafe_source = dir.path().join("unsafe.zip");
    build_zip(&unsafe_source, &[("../escape.txt", b"no")]);
    let (progress, cancel) = rig();
    assert!(matches!(
        convert_archive(
            &unsafe_source,
            "unsafe-converted",
            ConvertOptions {
                target: Format::Zip,
                level: Default::default(),
                input_password: None,
                output_password: None,
            },
            &progress,
            &cancel,
        ),
        Err(ArchiveError::ConversionUnsafeEntries(1))
    ));
    assert!(!dir.path().join("unsafe-converted.zip").exists());
    assert!(fs::read_dir(dir.path()).unwrap().flatten().all(|entry| {
        !entry
            .file_name()
            .to_string_lossy()
            .contains("ferail-convert")
    }));
}

#[test]
fn tar_gz_round_trip_streams_toc() {
    let tf = TempFile::new("bundle.tar.gz");
    {
        let enc = flate2::write::GzEncoder::new(
            File::create(tf.path()).unwrap(),
            flate2::Compression::default(),
        );
        let mut tb = tar::Builder::new(enc);
        for (name, data) in [
            ("proj/readme.md", &b"# hi"[..]),
            ("proj/src/main.rs", &b"fn main(){}"[..]),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tb.append_data(&mut header, name, data).unwrap();
        }
        tb.into_inner().unwrap().finish().unwrap();
    }

    let toc = read_toc(tf.path(), None).unwrap();
    assert_eq!(toc.file_count(), 2);
    assert_eq!(toc.single_root(), Some("proj"));
    // Tar records per-entry sizes, so the total is known.
    assert_eq!(toc.total_uncompressed(), Some(4 + 11));

    // Tar-family gets a format-only summary (no stream scan for a column).
    let summary = read_summary(tf.path()).unwrap();
    assert_eq!(summary.file_count, None);
}

#[test]
fn renamed_tar_gzip_is_probed_as_tar_and_lists_inner_entries() {
    let source = TempDir::new("renamed-tar-source");
    std::fs::write(source.path().join("inside.txt"), b"inside").unwrap();
    let archive = TempFile::new("download.tar (1).gz");
    let input = source.path().join("inside.txt");
    create_archive(
        Format::TarGz,
        &[input.as_path()],
        archive.path(),
        CreateOptions::default(),
        &TransferProgress::new(),
        &AtomicBool::new(false),
    )
    .unwrap();

    assert_eq!(probe_format(archive.path()), Some(Format::TarGz));
    let toc = read_toc(archive.path(), None).unwrap();
    assert!(toc.entries.iter().any(|entry| entry.path == "inside.txt"));
}

#[test]
fn single_member_gz_is_one_entry_named_without_suffix() {
    let tf = TempFile::new("report.csv.gz");
    {
        let mut enc = flate2::write::GzEncoder::new(
            File::create(tf.path()).unwrap(),
            flate2::Compression::default(),
        );
        enc.write_all(b"a,b,c\n1,2,3\n").unwrap();
        enc.finish().unwrap();
    }
    let toc = read_toc(tf.path(), None).unwrap();
    assert_eq!(toc.entries.len(), 1);
    let entry = &toc.entries[0];
    assert!(!entry.is_dir);
    // The `.gz` suffix is stripped to the logical member name. The temp name
    // is `...report.csv.gz`, so the member ends with `report.csv`.
    assert!(entry.path.ends_with("report.csv"), "got {}", entry.path);

    let summary = read_summary(tf.path()).unwrap();
    assert_eq!(summary.file_count, Some(1));
}

/// Build a zip from `(name, data)` pairs, writing names verbatim (so tests can
/// inject a malicious `../` entry).
fn build_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let opts = zip::write::SimpleFileOptions::default();
    let mut zw = zip::ZipWriter::new(File::create(path).unwrap());
    for (name, data) in entries {
        if name.ends_with('/') {
            zw.add_directory(*name, opts).unwrap();
        } else {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
    }
    zw.finish().unwrap();
}

#[test]
fn zip_extract_all_writes_real_files() {
    let tf = TempFile::new("extract.zip");
    build_zip(
        tf.path(),
        &[
            ("project/", b""),
            ("project/a.txt", b"hello"),
            ("project/sub/b.txt", b"world!!"),
        ],
    );
    let out = TempDir::new("extract-all");
    let (progress, cancel) = rig();
    let outcome = extract_all(
        tf.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &progress,
        &cancel,
    )
    .unwrap();

    assert_eq!(outcome.files_written, 2);
    assert!(outcome.skipped.is_empty());
    assert_eq!(
        fs::read_to_string(out.path().join("project/a.txt")).unwrap(),
        "hello"
    );
    assert_eq!(
        fs::read_to_string(out.path().join("project/sub/b.txt")).unwrap(),
        "world!!"
    );
    // The single created top-level entry is reported for undo/reveal.
    assert_eq!(outcome.created, vec![out.path().join("project")]);
}

#[test]
fn file_promise_materializes_exact_leaf_and_cleans_private_stage() {
    let archive = TempFile::new("promise.zip");
    build_zip(archive.path(), &[("nested/report.txt", b"promised")]);
    let destination = TempDir::new("promise-destination");
    let target = destination.path().join("report.txt");

    materialize_archive_entry(archive.path(), "nested/report.txt", &target, None).unwrap();

    assert_eq!(fs::read(&target).unwrap(), b"promised");
    let error =
        materialize_archive_entry(archive.path(), "nested/report.txt", &target, None).unwrap_err();
    assert!(matches!(error, ArchiveError::Io(_)));
    assert_eq!(fs::read(&target).unwrap(), b"promised");
    assert!(fs::read_dir(destination.path())
        .unwrap()
        .flatten()
        .all(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".ferail-archive-drag-")
        }));
}

#[test]
fn zip_cherry_pick_extracts_only_selected_subtree() {
    let tf = TempFile::new("cherry.zip");
    build_zip(
        tf.path(),
        &[
            ("keep/x.txt", b"x"),
            ("keep/deep/y.txt", b"y"),
            ("drop/z.txt", b"z"),
        ],
    );
    let out = TempDir::new("cherry");
    let (progress, cancel) = rig();
    // Select the `keep` directory subtree only.
    let outcome = extract_entries(
        tf.path(),
        out.path(),
        &["keep"],
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &progress,
        &cancel,
    )
    .unwrap();

    assert_eq!(outcome.files_written, 2);
    assert!(out.path().join("keep/x.txt").exists());
    assert!(out.path().join("keep/deep/y.txt").exists());
    assert!(!out.path().join("drop/z.txt").exists());
    assert!(!out.path().join("drop").exists());
}

#[test]
fn zip_slip_entry_is_skipped_not_written_outside_dest() {
    let tf = TempFile::new("evil.zip");
    build_zip(
        tf.path(),
        &[("../escape.txt", b"pwned"), ("safe.txt", b"ok")],
    );
    let out = TempDir::new("slip");
    let (progress, cancel) = rig();
    let outcome = extract_all(
        tf.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &progress,
        &cancel,
    )
    .unwrap();

    // The safe entry landed; the traversal entry was skipped and never written
    // to the parent of the destination.
    assert!(out.path().join("safe.txt").exists());
    assert_eq!(outcome.files_written, 1);
    assert!(outcome
        .skipped
        .iter()
        .any(|s| s.reason == SkipReason::UnsafePath));
    let escaped = out.path().parent().unwrap().join("escape.txt");
    assert!(!escaped.exists(), "traversal entry escaped to {escaped:?}");
}

#[cfg(unix)]
#[test]
fn extraction_never_follows_existing_directory_symlink() {
    use std::os::unix::fs::symlink;

    let archive = TempFile::new("directory-link.zip");
    build_zip(archive.path(), &[("linked/escape.txt", b"pwned")]);
    let out = TempDir::new("directory-link-out");
    let outside = TempDir::new("directory-link-target");
    symlink(outside.path(), out.path().join("linked")).unwrap();

    let (progress, cancel) = rig();
    let outcome = extract_all(
        archive.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &progress,
        &cancel,
    )
    .unwrap();

    assert!(!outside.path().join("escape.txt").exists());
    assert_eq!(outcome.files_written, 0);
    assert!(outcome
        .skipped
        .iter()
        .any(|entry| entry.reason == SkipReason::UnsafeDestinationLink));
}

#[cfg(unix)]
#[test]
fn extraction_never_follows_existing_file_symlink_even_when_overwriting() {
    use std::os::unix::fs::symlink;

    let archive = TempFile::new("file-link.zip");
    build_zip(archive.path(), &[("victim.txt", b"replacement")]);
    let out = TempDir::new("file-link-out");
    let outside = TempFile::new("file-link-target.txt");
    fs::write(outside.path(), b"original").unwrap();
    symlink(outside.path(), out.path().join("victim.txt")).unwrap();

    let (progress, cancel) = rig();
    let outcome = extract_all(
        archive.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &progress,
        &cancel,
    )
    .unwrap();

    assert_eq!(fs::read(outside.path()).unwrap(), b"original");
    assert_eq!(outcome.files_written, 0);
    assert!(outcome
        .skipped
        .iter()
        .any(|entry| entry.reason == SkipReason::UnsafeDestinationLink));
}

#[test]
fn tar_gz_extract_all_writes_real_files() {
    let tf = TempFile::new("bundle2.tar.gz");
    {
        let enc = flate2::write::GzEncoder::new(
            File::create(tf.path()).unwrap(),
            flate2::Compression::default(),
        );
        let mut tb = tar::Builder::new(enc);
        for (name, data) in [
            ("proj/readme.md", &b"# hi"[..]),
            ("proj/main.rs", &b"fn main(){}"[..]),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tb.append_data(&mut header, name, data).unwrap();
        }
        tb.into_inner().unwrap().finish().unwrap();
    }
    let out = TempDir::new("tar-extract");
    let (progress, cancel) = rig();
    let outcome = extract_all(
        tf.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &progress,
        &cancel,
    )
    .unwrap();
    assert_eq!(outcome.files_written, 2);
    assert_eq!(
        fs::read_to_string(out.path().join("proj/readme.md")).unwrap(),
        "# hi"
    );
}

#[test]
fn single_member_gz_extract_writes_decompressed_file() {
    let tf = TempFile::new("notes.txt.gz");
    {
        let mut enc = flate2::write::GzEncoder::new(
            File::create(tf.path()).unwrap(),
            flate2::Compression::default(),
        );
        enc.write_all(b"decompressed body").unwrap();
        enc.finish().unwrap();
    }
    let out = TempDir::new("single-extract");
    let (progress, cancel) = rig();
    let outcome = extract_all(
        tf.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &progress,
        &cancel,
    )
    .unwrap();
    assert_eq!(outcome.files_written, 1);
    // Member name is the temp leaf with `.gz` stripped, ending in `notes.txt`.
    let written = fs::read_dir(out.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(written.to_string_lossy().ends_with("notes.txt"));
    assert_eq!(fs::read_to_string(&written).unwrap(), "decompressed body");
}

/// Build `project/readme.md` + `project/src/main.rs` under `root`.
fn make_tree(root: &Path) {
    fs::create_dir_all(root.join("project/src")).unwrap();
    fs::write(root.join("project/readme.md"), b"# hi").unwrap();
    fs::write(root.join("project/src/main.rs"), b"fn main(){}").unwrap();
}

#[test]
fn zip_create_round_trips_through_extract() {
    let src = TempDir::new("src");
    make_tree(src.path());
    let arc = TempFile::new("created.zip");
    let input = src.path().join("project");
    let (progress, cancel) = rig();
    create_archive(
        Format::Zip,
        &[input.as_path()],
        arc.path(),
        CreateOptions::default(),
        &progress,
        &cancel,
    )
    .unwrap();

    // Keep-parent naming: entries are rooted at `project`.
    let toc = read_toc(arc.path(), None).unwrap();
    assert_eq!(toc.single_root(), Some("project"));
    assert_eq!(toc.file_count(), 2);

    let out = TempDir::new("out");
    let (p2, c2) = rig();
    extract_all(
        arc.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &p2,
        &c2,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(out.path().join("project/readme.md")).unwrap(),
        "# hi"
    );
    assert_eq!(
        fs::read_to_string(out.path().join("project/src/main.rs")).unwrap(),
        "fn main(){}"
    );
}

#[test]
fn zip_password_round_trip_requires_password_to_extract() {
    let src = TempDir::new("src-pw");
    make_tree(src.path());
    let arc = TempFile::new("secret.zip");
    let input = src.path().join("project");
    let (progress, cancel) = rig();
    create_archive(
        Format::Zip,
        &[input.as_path()],
        arc.path(),
        CreateOptions {
            password: Some("s3cret"),
            ..Default::default()
        },
        &progress,
        &cancel,
    )
    .unwrap();

    // The TOC reports it needs a password (metadata reads fine without one).
    let toc = read_toc(arc.path(), None).unwrap();
    assert!(toc.needs_password);

    // Extraction without the password is refused, not silently garbled.
    let out = TempDir::new("out-nopw");
    let (p2, c2) = rig();
    let err = extract_all(
        arc.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &p2,
        &c2,
    )
    .unwrap_err();
    assert!(matches!(err, ArchiveError::PasswordRequired), "got {err:?}");

    // With the password, content is recovered.
    let out2 = TempDir::new("out-pw");
    let (p3, c3) = rig();
    extract_all(
        arc.path(),
        out2.path(),
        ExtractOptions {
            password: Some("s3cret"),
            overwrite: true,
        },
        &p3,
        &c3,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(out2.path().join("project/readme.md")).unwrap(),
        "# hi"
    );
}

#[test]
fn tar_gz_create_round_trips_through_extract() {
    let src = TempDir::new("src-tgz");
    make_tree(src.path());
    let arc = TempFile::new("created.tar.gz");
    let input = src.path().join("project");
    let (progress, cancel) = rig();
    create_archive(
        Format::TarGz,
        &[input.as_path()],
        arc.path(),
        CreateOptions::default(),
        &progress,
        &cancel,
    )
    .unwrap();

    let out = TempDir::new("out-tgz");
    let (p2, c2) = rig();
    extract_all(
        arc.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &p2,
        &c2,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(out.path().join("project/src/main.rs")).unwrap(),
        "fn main(){}"
    );
}

#[test]
fn sevenz_round_trips_and_enriches_description() {
    // 7z has no pure-Rust writer in our engine, so build the fixture with the
    // backend's own writer (dev-only), then read it back through our reader.
    let src = TempDir::new("src-7z");
    make_tree(src.path());
    let arc = TempFile::new("bundle.7z");
    sevenz_rust::compress_to_path(src.path().join("project"), arc.path()).unwrap();

    // The reader (previously uncovered) lists the entries from the footer.
    let toc = read_toc(arc.path(), None).unwrap();
    assert!(
        toc.file_count() >= 2,
        "expected >=2 files, got {}",
        toc.file_count()
    );
    let summary = read_summary(arc.path()).unwrap();
    assert_eq!(summary.file_count, Some(toc.file_count() as u32));

    // End-to-end A-2b: magic detection enriches the 7z Description with the
    // file count instead of the bare "7-Zip archive" label.
    let info = crate::detect_magic_info(arc.path()).expect("7z detected");
    assert_eq!(info.magic_type, crate::MagicType::SevenZip);
    let desc = info.description();
    assert!(desc.starts_with("7-Zip archive"), "got {desc}");
    assert!(desc.contains("files"), "expected a file count in {desc:?}");
}

#[test]
fn sevenz_create_round_trips_through_extract() {
    let src = TempDir::new("src-7zc");
    make_tree(src.path());
    let arc = TempFile::new("made.7z");
    let input = src.path().join("project");
    let (progress, cancel) = rig();
    create_archive(
        Format::SevenZ,
        &[input.as_path()],
        arc.path(),
        CreateOptions::default(),
        &progress,
        &cancel,
    )
    .unwrap();

    let toc = read_toc(arc.path(), None).unwrap();
    assert_eq!(toc.single_root(), Some("project"));
    assert!(toc.file_count() >= 2);

    let out = TempDir::new("out-7zc");
    let (p2, c2) = rig();
    extract_all(
        arc.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &p2,
        &c2,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(out.path().join("project/readme.md")).unwrap(),
        "# hi"
    );
}

#[test]
fn sevenz_symlink_metadata_is_never_materialized() {
    let arc = TempFile::new("links.7z");
    let mut writer = sevenz_rust::SevenZWriter::create(arc.path()).unwrap();

    let mut link = sevenz_rust::SevenZArchiveEntry::new();
    link.name = "redirect".into();
    link.has_windows_attributes = true;
    link.windows_attributes = 0o120777 << 16;
    writer
        .push_archive_entry(link, Some(std::io::Cursor::new(b"../outside")))
        .unwrap();

    let mut safe = sevenz_rust::SevenZArchiveEntry::new();
    safe.name = "safe.txt".into();
    writer
        .push_archive_entry(safe, Some(std::io::Cursor::new(b"ok")))
        .unwrap();
    writer.finish().unwrap();

    let toc = read_toc(arc.path(), None).unwrap();
    assert_eq!(toc.entries[0].unix_mode, Some(0o120777));

    let out = TempDir::new("7z-link-out");
    let (progress, cancel) = rig();
    let outcome = extract_all(
        arc.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &progress,
        &cancel,
    )
    .unwrap();

    assert!(!out.path().join("redirect").exists());
    assert_eq!(fs::read(out.path().join("safe.txt")).unwrap(), b"ok");
    assert!(outcome
        .skipped
        .iter()
        .any(|entry| entry.reason == SkipReason::Symlink));
}

#[test]
fn single_member_create_rejects_multiple_inputs() {
    let src = TempDir::new("src-multi");
    make_tree(src.path());
    let arc = TempFile::new("bad.gz");
    let input = src.path().join("project"); // a directory → many files
    let (progress, cancel) = rig();
    let err = create_archive(
        Format::Gzip,
        &[input.as_path()],
        arc.path(),
        CreateOptions::default(),
        &progress,
        &cancel,
    )
    .unwrap_err();
    assert!(matches!(err, ArchiveError::Codec(_)), "got {err:?}");
}

#[test]
fn create_with_level_and_password_honours_options() {
    // Guards the New Archive dialog's contract: the CreateOptions it builds
    // (level + password) must reach the codec and be readable back.
    let src = TempDir::new("src-opts");
    make_tree(src.path());
    let input = src.path().join("project");

    // Maximum compression + a password → encrypted, and refuses to extract
    // without it.
    let secured = TempFile::new("opts.zip");
    let (p1, c1) = rig();
    create_archive(
        Format::Zip,
        &[input.as_path()],
        secured.path(),
        CreateOptions {
            level: ferail_archive::CompressionLevel::Maximum,
            password: Some("pw"),
        },
        &p1,
        &c1,
    )
    .unwrap();
    assert!(read_toc(secured.path(), None).unwrap().needs_password);

    // Store level → entries are not deflated, so the archive is at least as
    // large as its contents.
    let stored = TempFile::new("store.zip");
    let (p2, c2) = rig();
    create_archive(
        Format::Zip,
        &[input.as_path()],
        stored.path(),
        CreateOptions {
            level: ferail_archive::CompressionLevel::Store,
            password: None,
        },
        &p2,
        &c2,
    )
    .unwrap();
    let toc = read_toc(stored.path(), None).unwrap();
    assert!(!toc.needs_password);
    for e in toc.entries.iter().filter(|e| !e.is_dir) {
        assert_eq!(
            e.compressed_size, e.uncompressed_size,
            "Store level must not compress {}",
            e.path
        );
    }
}

#[test]
fn add_to_zip_appends_and_skips_existing_names() {
    let src = TempDir::new("src-add");
    make_tree(src.path());
    let arc = TempFile::new("addable.zip");
    let input = src.path().join("project");
    let (p0, c0) = rig();
    create_archive(
        Format::Zip,
        &[input.as_path()],
        arc.path(),
        CreateOptions::default(),
        &p0,
        &c0,
    )
    .unwrap();
    let before = read_toc(arc.path(), None).unwrap().file_count();

    // A genuinely new file lands.
    let extra_dir = TempDir::new("extra");
    let extra = extra_dir.path().join("notes.txt");
    fs::write(&extra, b"appended").unwrap();
    let (p1, c1) = rig();
    let outcome = super::add_to_archive(
        arc.path(),
        &[extra.as_path()],
        CreateOptions::default(),
        &p1,
        &c1,
    )
    .unwrap();
    assert_eq!(outcome.added, 1);
    assert!(outcome.skipped_existing.is_empty());

    let toc = read_toc(arc.path(), None).unwrap();
    assert_eq!(toc.file_count(), before + 1);
    assert!(toc.entries.iter().any(|e| e.path == "notes.txt"));

    // Adding the same name again is refused, not duplicated.
    let (p2, c2) = rig();
    let outcome = super::add_to_archive(
        arc.path(),
        &[extra.as_path()],
        CreateOptions::default(),
        &p2,
        &c2,
    )
    .unwrap();
    assert_eq!(outcome.added, 0);
    assert_eq!(outcome.skipped_existing, vec!["notes.txt".to_string()]);
    assert_eq!(read_toc(arc.path(), None).unwrap().file_count(), before + 1);

    // The appended content survives a real extraction.
    let out = TempDir::new("out-add");
    let (p3, c3) = rig();
    extract_all(
        arc.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &p3,
        &c3,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(out.path().join("notes.txt")).unwrap(),
        "appended"
    );
}

#[test]
fn staged_zip_edits_commit_as_one_transaction() {
    let arc = TempFile::new("staged.zip");
    build_zip(
        arc.path(),
        &[
            ("folder/a.txt", b"alpha"),
            ("keep.txt", b"keep"),
            ("remove.txt", b"remove"),
        ],
    );
    let source_dir = TempDir::new("staged-source");
    let source = source_dir.path().join("added.txt");
    fs::write(&source, b"added").unwrap();

    let expected = archive_stamp(arc.path()).unwrap();
    let plan = ArchiveEditPlan {
        additions: vec![ArchiveAddition {
            source,
            destination: "docs".to_string(),
        }],
        removals: vec!["remove.txt".to_string()],
        renames: vec![ArchiveRename {
            from: "folder".to_string(),
            to: "docs".to_string(),
        }],
    };
    let (progress, cancel) = rig();
    commit_archive_edits(
        arc.path(),
        expected,
        &plan,
        CreateOptions::default(),
        &progress,
        &cancel,
    )
    .unwrap();

    let toc = read_toc(arc.path(), None).unwrap();
    let names: Vec<&str> = toc
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert!(names.contains(&"docs/a.txt"), "got {names:?}");
    assert!(names.contains(&"docs/added.txt"), "got {names:?}");
    assert!(names.contains(&"keep.txt"), "got {names:?}");
    assert!(!names.contains(&"remove.txt"), "got {names:?}");

    let out = TempDir::new("staged-out");
    let (progress, cancel) = rig();
    extract_all(
        arc.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &progress,
        &cancel,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(out.path().join("docs/a.txt")).unwrap(),
        "alpha"
    );
    assert_eq!(
        fs::read_to_string(out.path().join("docs/added.txt")).unwrap(),
        "added"
    );
}

#[test]
fn cancelled_or_stale_zip_edit_never_changes_the_original() {
    let arc = TempFile::new("cancelled-edit.zip");
    build_zip(arc.path(), &[("original.txt", b"original")]);
    let original = fs::read(arc.path()).unwrap();
    let expected = archive_stamp(arc.path()).unwrap();
    let plan = ArchiveEditPlan {
        removals: vec!["original.txt".to_string()],
        ..ArchiveEditPlan::default()
    };

    let progress = TransferProgress::new();
    let cancel = AtomicBool::new(true);
    assert!(matches!(
        commit_archive_edits(
            arc.path(),
            expected,
            &plan,
            CreateOptions::default(),
            &progress,
            &cancel,
        ),
        Err(ArchiveError::Cancelled)
    ));
    assert_eq!(fs::read(arc.path()).unwrap(), original);

    // A changed length makes the stale-workbench guard deterministic even on
    // filesystems whose timestamp resolution is coarse.
    fs::OpenOptions::new()
        .append(true)
        .open(arc.path())
        .unwrap()
        .write_all(b"external change")
        .unwrap();
    let changed = fs::read(arc.path()).unwrap();
    let (progress, cancel) = rig();
    assert!(matches!(
        commit_archive_edits(
            arc.path(),
            expected,
            &plan,
            CreateOptions::default(),
            &progress,
            &cancel,
        ),
        Err(ArchiveError::Codec(_))
    ));
    assert_eq!(fs::read(arc.path()).unwrap(), changed);
}

#[test]
fn add_to_tar_is_refused() {
    let src = TempDir::new("src-addtar");
    make_tree(src.path());
    let arc = TempFile::new("nope.tar.gz");
    let input = src.path().join("project");
    let (p0, c0) = rig();
    create_archive(
        Format::TarGz,
        &[input.as_path()],
        arc.path(),
        CreateOptions::default(),
        &p0,
        &c0,
    )
    .unwrap();
    let (p1, c1) = rig();
    let err = super::add_to_archive(
        arc.path(),
        &[input.as_path()],
        CreateOptions::default(),
        &p1,
        &c1,
    )
    .unwrap_err();
    assert!(matches!(err, ArchiveError::Codec(_)), "got {err:?}");
}

#[test]
fn zip_container_packages_are_browseable_but_never_rewritten() {
    let package = TempFile::new("document.docx");
    build_zip(package.path(), &[("[Content_Types].xml", b"<Types/>")]);
    assert_eq!(read_toc(package.path(), None).unwrap().file_count(), 1);

    let original = fs::read(package.path()).unwrap();
    let plan = ArchiveEditPlan {
        removals: vec!["[Content_Types].xml".to_string()],
        ..ArchiveEditPlan::default()
    };
    let (progress, cancel) = rig();
    assert!(matches!(
        commit_archive_edits(
            package.path(),
            archive_stamp(package.path()).unwrap(),
            &plan,
            CreateOptions::default(),
            &progress,
            &cancel,
        ),
        Err(ArchiveError::Codec(_))
    ));
    assert_eq!(fs::read(package.path()).unwrap(), original);
}

#[test]
fn probe_detects_zip_containers_by_content_not_extension() {
    // A file whose extension says nothing about being an archive: the
    // shape of every .docx/.xlsx/.jar/.apk: must still open as a zip.
    let disguised = TempFile::new("report.docx");
    build_zip(
        disguised.path(),
        &[
            ("[Content_Types].xml", b"<xml/>"),
            ("word/document.xml", b"<w/>"),
        ],
    );
    assert_eq!(super::probe_format(disguised.path()), Some(Format::Zip));
    // And the whole read path works through it, not just detection.
    let toc = read_toc(disguised.path(), None).unwrap();
    assert_eq!(toc.file_count(), 2);

    // A genuinely non-archive file is reported as such rather than opened
    // as an empty archive.
    let plain = TempFile::new("notes.txt");
    fs::write(plain.path(), b"just some text, definitely not a zip").unwrap();
    assert_eq!(super::probe_format(plain.path()), None);
    assert!(matches!(
        read_toc(plain.path(), None),
        Err(ArchiveError::UnsupportedFormat)
    ));
}

#[test]
fn zip_entries_carry_real_modification_times() {
    // Guards the DOS-timestamp conversion: a freshly written zip must report a
    // time close to now, not the epoch (which is what the Modified column
    // showed before the conversion existed).
    let tf = TempFile::new("stamped.zip");
    build_zip(tf.path(), &[("a.txt", b"hi")]);
    let toc = read_toc(tf.path(), None).unwrap();
    let stamp = toc.entries[0].mtime_unix.expect("zip records a timestamp");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    // DOS timestamps have 2-second granularity and no timezone; allow a wide
    // window (a day) so the assertion is about "decoded sanely", not clock skew.
    let skew = (now - stamp).abs();
    assert!(
        skew < 86_400,
        "decoded {stamp} is {skew}s from now ({now}): conversion looks wrong"
    );
    // Pin the arithmetic itself against a known value: the DOS epoch
    // (1980-01-01T00:00:00Z) is 315_532_800 in unix seconds.
    // Grouped by the MS-DOS date field layout (7-bit year-since-1980, 4-bit
    // month, 5-bit day), not in even nibbles: the field boundaries are the
    // point of the literal, so the "unusual grouping" here is deliberate.
    #[allow(clippy::unusual_byte_groupings)]
    let dos_epoch = zip::DateTime::try_from_msdos(0b0000000_0001_00001, 0).unwrap();
    assert_eq!(
        super::zip_codec::dos_datetime_to_unix(dos_epoch),
        Some(315_532_800)
    );
}

/// Build a level-0 LHA archive containing `-lh0-` (stored) members.
///
/// Written by hand rather than shipped as a binary fixture: `delharc` decodes
/// but cannot compress, so there is no way to generate one in-tree, and a
/// checked-in blob would be unreviewable. Stored members keep the encoder
/// trivial, the payload is the file's own bytes, while still exercising the
/// real header parser, CRC check and streaming walk.
///
/// Level-0 header layout (LHA spec):
///   0   u8    header size (bytes 2..=header_end)
///   1   u8    header checksum (sum of bytes 2..=header_end, mod 256)
///   2   [5]   method id, e.g. `-lh0-`
///   7   u32le compressed size
///   11  u32le original size
///   15  u32le MS-DOS timestamp
///   19  u8    file attribute (0x20 = archived)
///   20  u8    header level (0)
///   21  u8    filename length
///   22  [n]   filename
///   22+n u16le CRC-16 of the *uncompressed* data
fn build_lha(path: &Path, entries: &[(&str, &[u8])]) {
    fn crc16(data: &[u8]) -> u16 {
        // CRC-16/ARC, the variant LHA uses: reflected, poly 0xA001, init 0.
        let mut crc: u16 = 0;
        for &b in data {
            crc ^= b as u16;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xA001
                } else {
                    crc >> 1
                };
            }
        }
        crc
    }

    let mut out: Vec<u8> = Vec::new();
    for (name, data) in entries {
        let name_bytes = name.as_bytes();
        // Everything from the method id to the CRC, i.e. the bytes the size
        // and checksum fields describe.
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(b"-lh0-");
        body.extend_from_slice(&(data.len() as u32).to_le_bytes()); // compressed
        body.extend_from_slice(&(data.len() as u32).to_le_bytes()); // original
                                                                    // 1980-01-01 00:00:00 in MS-DOS packed form (date << 16 | time).
        body.extend_from_slice(&0x0021_0000u32.to_le_bytes());
        body.push(0x20); // attribute: archived
        body.push(0x00); // header level 0
        body.push(name_bytes.len() as u8);
        body.extend_from_slice(name_bytes);
        body.extend_from_slice(&crc16(data).to_le_bytes());

        out.push(body.len() as u8);
        out.push(body.iter().fold(0u8, |acc, b| acc.wrapping_add(*b)));
        out.extend_from_slice(&body);
        out.extend_from_slice(data);
    }
    // A zero header-size byte terminates the archive.
    out.push(0);
    fs::write(path, out).unwrap();
}

#[test]
fn lha_round_trips_toc_and_extract() {
    let tf = TempFile::new("amiga.lha");
    build_lha(
        tf.path(),
        &[
            ("readme.txt", b"hello aminet"),
            ("data/notes.txt", b"second entry"),
        ],
    );

    let toc = read_toc(tf.path(), None).unwrap();
    assert_eq!(toc.entries.len(), 2);
    assert!(!toc.needs_password);
    assert_eq!(toc.entries[0].path, "readme.txt");
    assert_eq!(toc.entries[0].uncompressed_size, Some(12));
    assert!(!toc.entries[0].is_dir);
    assert_eq!(toc.entries[1].path, "data/notes.txt");

    let out = TempDir::new("lha-extract");
    let (progress, cancel) = rig();
    let outcome = extract_all(
        tf.path(),
        out.path(),
        ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
        &progress,
        &cancel,
    )
    .unwrap();

    assert_eq!(outcome.files_written, 2);
    assert!(
        outcome.skipped.is_empty(),
        "unexpected skips: {:?}",
        outcome.skipped
    );
    assert_eq!(
        fs::read_to_string(out.path().join("readme.txt")).unwrap(),
        "hello aminet"
    );
    assert_eq!(
        fs::read_to_string(out.path().join("data/notes.txt")).unwrap(),
        "second entry"
    );
}

#[test]
fn lha_is_detected_by_content_without_an_extension() {
    // Aminet downloads routinely arrive with the extension stripped or
    // renamed, so the magic table has to carry the format on its own.
    let tf = TempFile::new("no-extension-here");
    build_lha(tf.path(), &[("a.txt", b"x")]);
    assert_eq!(super::probe_format(tf.path()), Some(Format::Lha));
}

#[test]
fn lha_is_read_only_and_absent_from_the_create_picker() {
    // delharc decodes but does not compress; the capability matrix is what
    // keeps Create Archive from offering a format we cannot write.
    let caps = Format::Lha.capabilities();
    assert!(caps.can_browse && caps.can_extract);
    assert!(!caps.can_create, "LHA has no writer");
    assert!(caps.is_read_only());
    assert!(!Format::creatable_multi_file().contains(&Format::Lha));
}

/// Review finding 1: an in-place append could leave a truncated member inside
/// the user's real archive when the operation failed part-way. Adding is now
/// staged and swapped in atomically, so a cancelled add leaves the original
/// byte-for-byte intact and drops no temp files.
#[test]
fn cancelled_add_leaves_the_original_archive_untouched() {
    let src = TempDir::new("src-add-cancel");
    make_tree(src.path());
    let arc = TempFile::new("cancel-add.zip");
    let input = src.path().join("project");
    let (p0, c0) = rig();
    create_archive(
        Format::Zip,
        &[input.as_path()],
        arc.path(),
        CreateOptions::default(),
        &p0,
        &c0,
    )
    .unwrap();
    let before_bytes = fs::read(arc.path()).unwrap();

    let extra_dir = TempDir::new("extra-cancel");
    let extra = extra_dir.path().join("late.txt");
    fs::write(&extra, vec![b'x'; 512 * 1024]).unwrap();

    let (progress, cancel) = rig();
    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = super::add_to_archive(
        arc.path(),
        &[extra.as_path()],
        CreateOptions::default(),
        &progress,
        &cancel,
    );
    assert!(matches!(result, Err(ArchiveError::Cancelled)));

    assert_eq!(
        fs::read(arc.path()).unwrap(),
        before_bytes,
        "a cancelled add must not modify the archive"
    );
    // The staging sibling is cleaned up rather than left beside the archive.
    // Scoped to this archive's own name: the temp parent is shared with other
    // tests, whose in-flight staging files are none of this test's business.
    let parent = arc.path().parent().unwrap();
    let leaf = arc
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let leftovers: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("ferail-add") && name.contains(&leaf))
        .collect();
    assert!(leftovers.is_empty(), "left staging files: {leftovers:?}");
}

/// Review finding 3: duplicate detection consulted only the names that existed
/// before the operation, so two inputs resolving to the same archive path could
/// both be written: zip allows that, but the second record shadows the first.
#[test]
fn adding_two_inputs_with_the_same_leaf_keeps_only_the_first() {
    let src = TempDir::new("src-dup-leaf");
    make_tree(src.path());
    let arc = TempFile::new("dup-leaf.zip");
    let (p0, c0) = rig();
    create_archive(
        Format::Zip,
        &[src.path().join("project").as_path()],
        arc.path(),
        CreateOptions::default(),
        &p0,
        &c0,
    )
    .unwrap();

    // Two different files that both want to be `notes.txt` in the archive.
    let a_dir = TempDir::new("dup-a");
    let b_dir = TempDir::new("dup-b");
    let a = a_dir.path().join("notes.txt");
    let b = b_dir.path().join("notes.txt");
    fs::write(&a, b"first").unwrap();
    fs::write(&b, b"second").unwrap();

    let (progress, cancel) = rig();
    let outcome = super::add_to_archive(
        arc.path(),
        &[a.as_path(), b.as_path()],
        CreateOptions::default(),
        &progress,
        &cancel,
    )
    .unwrap();
    assert_eq!(outcome.added, 1);
    assert_eq!(outcome.skipped_existing, vec!["notes.txt".to_string()]);

    let toc = read_toc(arc.path(), None).unwrap();
    let matches: Vec<_> = toc
        .entries
        .iter()
        .filter(|e| e.path == "notes.txt")
        .collect();
    assert_eq!(matches.len(), 1, "duplicate record written");
}

/// Review finding 2: a local name may legitimately contain a backslash on
/// Unix. Translating it to `/` invented archive structure: the real file
/// `..\payload` became the entry `../payload`, a traversal path Ferail would
/// refuse on extraction but another tool might not. Ferail must never write
/// one.
#[cfg(unix)]
#[test]
fn a_local_name_with_a_backslash_never_becomes_a_traversal_entry() {
    let src = TempDir::new("src-backslash");
    fs::create_dir_all(src.path()).ok();
    let sneaky = src.path().join("..\\payload");
    fs::write(&sneaky, b"payload").unwrap();
    let ordinary = src.path().join("ordinary.txt");
    fs::write(&ordinary, b"fine").unwrap();

    let arc = TempFile::new("backslash.zip");
    let (progress, cancel) = rig();
    create_archive(
        Format::Zip,
        &[sneaky.as_path(), ordinary.as_path()],
        arc.path(),
        CreateOptions::default(),
        &progress,
        &cancel,
    )
    .unwrap();

    let toc = read_toc(arc.path(), None).unwrap();
    for entry in &toc.entries {
        assert!(
            !entry.path.contains("../"),
            "archive contains a traversal path: {}",
            entry.path
        );
        assert!(
            ferail_archive::safe_relative_path(&entry.path).is_ok(),
            "archive contains an unsafe path: {}",
            entry.path
        );
    }
    // The safe sibling still made it in.
    assert!(toc.entries.iter().any(|e| e.path == "ordinary.txt"));
}

/// Review finding 5: dropping archive members on a ZIP row adds them to that
/// ZIP. The GUI worker does exactly this: cherry-pick the members into a
/// private staging directory, then append them by leaf name, so the two
/// primitives are exercised here in the same order, including a member picked
/// out of an inner folder (which must land beside its new siblings, not
/// recreate the folder).
#[test]
fn members_extracted_from_one_archive_can_be_appended_to_another() {
    let src = TempDir::new("member-src");
    let root = src.path().join("payload");
    fs::create_dir_all(root.join("inner")).unwrap();
    fs::write(root.join("alpha.txt"), b"member-alpha").unwrap();
    fs::write(root.join("inner/gamma.txt"), b"member-gamma").unwrap();

    let source = TempFile::new("member-source.zip");
    let (p0, c0) = rig();
    create_archive(
        Format::Zip,
        &[root.as_path()],
        source.path(),
        CreateOptions::default(),
        &p0,
        &c0,
    )
    .unwrap();

    let target = TempFile::new("member-target.zip");
    let seed_dir = TempDir::new("member-seed");
    let seed = seed_dir.path().join("seed.txt");
    fs::write(&seed, b"seed").unwrap();
    let (p1, c1) = rig();
    create_archive(
        Format::Zip,
        &[seed.as_path()],
        target.path(),
        CreateOptions::default(),
        &p1,
        &c1,
    )
    .unwrap();

    // 1. Cherry-pick the dragged members into staging.
    let staging = TempDir::new("member-staging");
    let entries = ["payload/alpha.txt", "payload/inner/gamma.txt"];
    let (p2, c2) = rig();
    super::extract_entries(
        source.path(),
        staging.path(),
        &entries,
        ExtractOptions::default(),
        &p2,
        &c2,
    )
    .unwrap();

    // 2. Append them by leaf, the way the drop does.
    let staged: Vec<std::path::PathBuf> = entries
        .iter()
        .map(|e| {
            e.split('/')
                .fold(staging.path().to_path_buf(), |a, p| a.join(p))
        })
        .collect();
    assert!(staged.iter().all(|p| p.exists()), "staging incomplete");
    let refs: Vec<&Path> = staged.iter().map(|p| p.as_path()).collect();
    let (p3, c3) = rig();
    let outcome =
        super::add_to_archive(target.path(), &refs, CreateOptions::default(), &p3, &c3).unwrap();
    assert_eq!(outcome.added, 2);

    let toc = read_toc(target.path(), None).unwrap();
    let names: Vec<&str> = toc.entries.iter().map(|e| e.path.as_str()).collect();
    assert!(
        names.contains(&"seed.txt"),
        "original member lost: {names:?}"
    );
    assert!(names.contains(&"alpha.txt"), "{names:?}");
    // Leaf, not `payload/inner/gamma.txt`.
    assert!(names.contains(&"gamma.txt"), "{names:?}");
}
