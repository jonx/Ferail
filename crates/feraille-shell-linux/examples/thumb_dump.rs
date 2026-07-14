//! Generate a content thumbnail via the freedesktop cache path and dump it to
//! `<out>.rgba.png` so it can be eyeballed off-GUI.
//!
//! ```sh
//! cargo run -p feraille-shell-linux --example thumb_dump -- <image> <out.png>
//! ```
//! Linux-only (the thumbnail path is target-gated); a no-op elsewhere.

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("thumb_dump is Linux-only.");
}

#[cfg(target_os = "linux")]
fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().expect("usage: thumb_dump <image> <out.png>");
    let out = args.next().expect("usage: thumb_dump <image> <out.png>");
    match feraille_shell_linux::fetch_quick_look_thumbnail(std::path::Path::new(&input), 256) {
        Some((rgba, w, h)) => {
            let opaque = rgba.chunks_exact(4).filter(|px| px[3] != 0).count();
            image::save_buffer(&out, &rgba, w, h, image::ColorType::Rgba8).unwrap();
            println!("thumbnail {w}x{h} ({opaque} opaque px) -> {out}");
        }
        None => println!("no thumbnail produced for {input}"),
    }
}
