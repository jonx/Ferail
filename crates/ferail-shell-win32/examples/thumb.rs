//! Standalone thumbnail extraction test.
//!
//! Usage:
//!     cargo run -p ferail-shell-win32 --example thumb -- <path> [out.png] [size]
//!
//! Calls `fetch_quick_look_thumbnail` and dumps the result to disk so
//! we can eyeball the pipeline without launching the GUI. Defaults:
//! out = "thumb.png", size = 512.

#[cfg(windows)]
fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: thumb <path> [out.png] [size]");
        std::process::exit(2);
    };
    let out = args.next().unwrap_or_else(|| "thumb.png".to_string());
    let size: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(512);

    let path = std::path::PathBuf::from(&path);
    println!("input : {}", path.display());
    println!("size  : {} px", size);

    match ferail_shell_win32::fetch_quick_look_thumbnail(&path, size) {
        Some((rgba, w, h)) => {
            println!("ok    : {}x{} ({} bytes)", w, h, rgba.len());
            // Sanity stats so we can tell black/transparent failures
            // apart from real images.
            let mut sum_alpha: u64 = 0;
            let mut sum_rgb: u64 = 0;
            let mut nonzero_rgb: u64 = 0;
            for px in rgba.chunks_exact(4) {
                sum_rgb += px[0] as u64 + px[1] as u64 + px[2] as u64;
                sum_alpha += px[3] as u64;
                if px[0] != 0 || px[1] != 0 || px[2] != 0 {
                    nonzero_rgb += 1;
                }
            }
            let n_px = (w as u64) * (h as u64);
            println!(
                "stats : mean RGB={:.1}/255  mean A={:.1}/255  non-black px={}/{}",
                sum_rgb as f64 / (n_px as f64 * 3.0),
                sum_alpha as f64 / n_px as f64,
                nonzero_rgb,
                n_px
            );

            let img = image::RgbaImage::from_raw(w, h, rgba).expect("rgba dims match");
            img.save(&out).expect("save png");
            println!("wrote : {}", out);
        }
        None => {
            eprintln!("error : fetch_quick_look_thumbnail returned None");
            std::process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("This example is Windows-only.");
    std::process::exit(2);
}
