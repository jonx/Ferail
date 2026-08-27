#[cfg(windows)]
fn main() {
    let mut args = std::env::args_os();
    let _exe = args.next();
    let first = args.next();
    if first.as_deref() == Some(std::ffi::OsStr::new("--diagnose")) {
        let Some(path) = args.next() else {
            eprintln!("usage: ferail-ntfs-helper --diagnose <local-ntfs-path>");
            std::process::exit(2);
        };
        if args.next().is_some() {
            eprintln!("expected exactly one local NTFS path");
            std::process::exit(2);
        }
        std::process::exit(ferail_ntfs_win32::run_diagnostic(&path));
    }
    if matches!(first.as_deref(), Some(arg) if arg == std::ffi::OsStr::new("--help") || arg == std::ffi::OsStr::new("-h"))
    {
        println!("Ferail Fast NTFS elevated helper");
        println!("usage: ferail-ntfs-helper --diagnose <local-ntfs-path>");
        println!("run the diagnostic from an elevated PowerShell");
        println!("output contains aggregate counts and timings only; no names or paths");
        std::process::exit(0);
    }
    std::process::exit(ferail_ntfs_win32::helper_main());
}

#[cfg(not(windows))]
fn main() {
    std::process::exit(2);
}
