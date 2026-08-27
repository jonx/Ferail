#[cfg(windows)]
fn main() {
    std::process::exit(ferail_ntfs_win32::helper_main());
}

#[cfg(not(windows))]
fn main() {
    std::process::exit(2);
}
