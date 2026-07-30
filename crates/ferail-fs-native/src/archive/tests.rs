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
    create_archive, extract_all, extract_entries, format_of, read_summary, read_toc, ArchiveError,
    CreateOptions, ExtractOptions, SkipReason,
};
use crate::file_ops::TransferProgress;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A unique temp directory that recursively removes itself on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!("ferail-archive-dir-{}-{}-{}", std::process::id(), n, tag));
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
    assert_eq!(format_of(std::path::Path::new("x.zip")).unwrap(), Format::Zip);
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
    assert!(!toc.needs_password);

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
fn tar_gz_round_trip_streams_toc() {
    let tf = TempFile::new("bundle.tar.gz");
    {
        let enc = flate2::write::GzEncoder::new(
            File::create(tf.path()).unwrap(),
            flate2::Compression::default(),
        );
        let mut tb = tar::Builder::new(enc);
        for (name, data) in [("proj/readme.md", &b"# hi"[..]), ("proj/src/main.rs", &b"fn main(){}"[..])] {
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

#[test]
fn tar_gz_extract_all_writes_real_files() {
    let tf = TempFile::new("bundle2.tar.gz");
    {
        let enc = flate2::write::GzEncoder::new(
            File::create(tf.path()).unwrap(),
            flate2::Compression::default(),
        );
        let mut tb = tar::Builder::new(enc);
        for (name, data) in [("proj/readme.md", &b"# hi"[..]), ("proj/main.rs", &b"fn main(){}"[..])] {
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
    assert!(toc.file_count() >= 2, "expected >=2 files, got {}", toc.file_count());
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
fn probe_detects_zip_containers_by_content_not_extension() {
    // A file whose extension says nothing about being an archive — the
    // shape of every .docx/.xlsx/.jar/.apk — must still open as a zip.
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
        "decoded {stamp} is {skew}s from now ({now}) — conversion looks wrong"
    );
    // Pin the arithmetic itself against a known value: the DOS epoch
    // (1980-01-01T00:00:00Z) is 315_532_800 in unix seconds.
    let dos_epoch = zip::DateTime::try_from_msdos(0b0000000_0001_00001, 0).unwrap();
    assert_eq!(super::zip_codec::dos_datetime_to_unix(dos_epoch), Some(315_532_800));
}
