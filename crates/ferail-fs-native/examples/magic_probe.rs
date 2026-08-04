fn main() {
    for arg in std::env::args().skip(1) {
        let p = std::path::Path::new(&arg);
        match ferail_fs_native::detect_magic_info(p) {
            Some(info) => println!(
                "{arg}\n  type={:?} label={:?} description={:?}",
                info.magic_type,
                info.magic_type.display_name(),
                info.description()
            ),
            None => println!("{arg}\n  <no detection>"),
        }
    }
}
