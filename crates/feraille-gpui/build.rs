// Windows: bake the .ico + a VERSIONINFO block into the .exe's
// resource section so Explorer, taskbar, Alt-Tab, the title bar,
// and "Pin to Start" all pick up our icon and metadata without any
// runtime call. macOS / Linux: this build script is a no-op.

fn main() {
    println!("cargo:rerun-if-changed=resources/feraille.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("resources/feraille.ico")
            .set("ProductName", "Feraille")
            .set("FileDescription", "Feraille file explorer")
            .set("CompanyName", "John Knipper")
            .set("LegalCopyright", "Copyright \u{00A9} 2026 John Knipper")
            .set("OriginalFilename", "feraille-gpui.exe")
            .set("InternalName", "feraille-gpui");
        if let Err(e) = res.compile() {
            // Don't fail the build on hosts without a usable
            // resource compiler (the windows-msvc toolchain ships
            // rc.exe; the windows-gnu toolchain ships windres). Log
            // and continue — the .exe just won't have its icon.
            println!("cargo:warning=winresource compile failed: {e}");
        }
    }
}
