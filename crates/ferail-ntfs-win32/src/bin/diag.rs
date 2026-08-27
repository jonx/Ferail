#[cfg(windows)]
fn main() {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: ferail-ntfs-diag <local-ntfs-path>");
        eprintln!("run this diagnostic from an elevated terminal");
        std::process::exit(2);
    };
    if std::env::args_os().nth(2).is_some() {
        eprintln!("expected exactly one local NTFS path");
        std::process::exit(2);
    }
    std::process::exit(ferail_ntfs_win32::run_diagnostic(&path));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ferail-ntfs-diag is available only on Windows");
    std::process::exit(2);
}
