// Windows: bake the .ico + a VERSIONINFO block into the .exe's
// resource section so Explorer, taskbar, Alt-Tab, the title bar,
// and "Pin to Start" all pick up our icon and metadata without any
// runtime call. macOS / Linux: this build script is a no-op.

fn main() {
    println!("cargo:rerun-if-changed=resources/ferail.ico");
    println!("cargo:rerun-if-changed=build.rs");

    emit_helper_attestation();

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("resources/ferail.ico")
            .set("ProductName", "Ferail")
            .set("FileDescription", "Ferail file explorer")
            .set("CompanyName", "John Knipper")
            .set("LegalCopyright", "Copyright \u{00A9} 2026 John Knipper")
            .set("OriginalFilename", "ferail-gpui.exe")
            .set("InternalName", "ferail-gpui");
        if let Err(e) = res.compile() {
            // Don't fail the build on hosts without a usable
            // resource compiler (the windows-msvc toolchain ships
            // rc.exe; the windows-gnu toolchain ships windres). Log
            // and continue: the .exe just won't have its icon.
            println!("cargo:warning=winresource compile failed: {e}");
        }
    }
}

/// Bake the expected Fast NTFS helper identity into this binary's constant
/// data.
///
/// `scripts/package-win.ps1` builds `ferail-ntfs-helper.exe` first, draws a
/// fresh 32-byte salt, hashes the helper as `SHA-256(salt ‖ file ‖ salt)`, and
/// exports both as environment variables before building this crate. The
/// values therefore land in `.rdata` like any other constant, there is no
/// sidecar file to edit and no magic marker to search for.
///
/// An ordinary `cargo build` sets neither variable, so a development tree
/// emits `None` and the launch path runs unattested (and says so). The
/// packaging script refuses to publish a release in that state.
fn emit_helper_attestation() {
    println!("cargo:rerun-if-env-changed=FERAIL_NTFS_HELPER_SALT");
    println!("cargo:rerun-if-env-changed=FERAIL_NTFS_HELPER_DIGEST");

    let salt = std::env::var("FERAIL_NTFS_HELPER_SALT").ok();
    let digest = std::env::var("FERAIL_NTFS_HELPER_DIGEST").ok();
    let body = match (salt.as_deref(), digest.as_deref()) {
        (Some(salt), Some(digest)) if is_hex32(salt) && is_hex32(digest) => format!(
            "pub const HELPER_ATTESTATION: Option<(&str, &str)> = Some((\"{salt}\", \"{digest}\"));\n"
        ),
        (None, None) => "pub const HELPER_ATTESTATION: Option<(&str, &str)> = None;\n".to_string(),
        _ => {
            // Half-configured is a packaging mistake, not a valid state: fail
            // the build rather than silently shipping an unverified helper
            // launch that looks verified.
            panic!(
                "FERAIL_NTFS_HELPER_SALT and FERAIL_NTFS_HELPER_DIGEST must both be set to 64 hex characters, or both be unset"
            );
        }
    };

    let out = std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR"))
        .join("helper_attestation.rs");
    std::fs::write(&out, body).expect("write helper attestation");
}

fn is_hex32(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}
