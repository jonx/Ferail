//! Dump per-file OS/theme icons to PNGs so they can be eyeballed off-GUI.
//!
//! ```sh
//! cargo run -p ferail-fs-native --example icon_dump -- /tmp/out
//! ```
//! Writes `<out>/<label>.png` for a handful of representative file types.
//! Linux-only (uses the `image` crate that backs the Linux icon rasterizer);
//! a no-op on other hosts so the workspace `--examples` build stays green.

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("icon_dump is Linux-only (needs the freedesktop icon theme).");
}

#[cfg(target_os = "linux")]
fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
    let out = std::path::Path::new(&out);
    std::fs::create_dir_all(out).unwrap();

    // (label, filename, bytes) — the extension drives the MIME/theme lookup.
    let samples: &[(&str, &str, &[u8])] = &[
        ("text", "note.txt", b"hello"),
        ("rust", "main.rs", b"fn main() {}"),
        ("pdf", "doc.pdf", b"%PDF-1.4"),
        ("png", "pic.png", b"\x89PNG\r\n\x1a\n"),
        ("json", "data.json", b"{}"),
        ("html", "page.html", b"<html>"),
    ];

    let tmp = out.join("srcfiles");
    std::fs::create_dir_all(&tmp).unwrap();
    for (label, name, bytes) in samples {
        let p = tmp.join(name);
        std::fs::write(&p, bytes).unwrap();
        dump(label, &p, out);
    }
    dump("folder", &tmp, out);
}

#[cfg(target_os = "linux")]
fn dump(label: &str, path: &std::path::Path, out: &std::path::Path) {
    match ferail_fs_native::fetch_icon_rgba(path, 64) {
        Some((rgba, w, h)) => {
            let opaque = rgba.chunks_exact(4).filter(|px| px[3] != 0).count();
            let png = out.join(format!("{label}.png"));
            image::save_buffer(&png, &rgba, w, h, image::ColorType::Rgba8).unwrap();
            println!(
                "{label:6} -> {} ({w}x{h}, {opaque} opaque px)",
                png.display()
            );
        }
        None => println!("{label:6} -> None (no icon resolved)"),
    }
}
