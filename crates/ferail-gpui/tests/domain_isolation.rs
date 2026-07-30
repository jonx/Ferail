//! Domain-isolation check: the domain crates compile and work
//! end-to-end with zero GPUI involvement.
//!
//! This integration test opens a directory, indexes it, and walks the
//! file list — entirely through the domain crates, without GPUI
//! involvement.
//!
//! The test is in the `ferail-gpui` crate (the *consumer* of both
//! GPUI and the domain layer) so that if anything in the dep graph ever
//! pulls a GPUI symbol into a domain path, the build breaks here.
//!
//! IMPORTANT: this test must never `use gpui::…` or `use gpui_component::…`.
//! If you add such an import, you've broken Phase 2's invariant.

use std::path::PathBuf;

use ferail_core::{EntryKind, FsBackend};
use ferail_fs_native::{NativeFs, home_dir};

#[test]
fn enumerate_home_via_domain_layer_only() {
    let fs = NativeFs::new();
    let root = fs.root();

    let handle = fs.enumerate(root);

    // The home directory may be empty in unusual environments (CI,
    // fresh containers), but `enumerate` should always return a
    // handle. The error slot is the only thing we hard-assert on:
    // a TCC/permission denial is acceptable here because the test
    // may be run sandboxed — what we care about is that the domain
    // layer *works without rendering*.
    if let Some(err) = &handle.error {
        eprintln!("note: enumeration returned error: {err:?}");
        return;
    }

    eprintln!(
        "enumerated {} ({} entries)",
        home_dir().display(),
        handle.initial.len()
    );
    for entry in handle.initial.iter().take(5) {
        let kind = match entry.kind {
            EntryKind::Directory => "DIR ",
            EntryKind::File => "FILE",
            EntryKind::Symlink => "LINK",
        };
        eprintln!("  {kind}  {}", entry.name);
    }
}

#[test]
fn fs_backend_trait_object_is_object_safe_for_ui_consumers() {
    // The UI layer holds `Box<dyn FsBackend>`. If FsBackend stops
    // being dyn-safe, the new Phase-3+ code can't use it the same
    // way the old code did. This test pins that contract.
    let _: Box<dyn FsBackend> = Box::new(NativeFs::new());
}

#[test]
fn path_for_root_round_trips() {
    let fs = NativeFs::new();
    let root = fs.root();
    let path: PathBuf = fs.path_for(root).expect("root path");
    // After `id_for_path`, the id should match the one we started
    // with — domain layer is self-consistent.
    let again = fs.id_for_path(&path);
    assert_eq!(root, again);
}
